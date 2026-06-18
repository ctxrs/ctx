use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;

use ctx_daemon::daemon::PluginInventoryHandle;
use ctx_route_contracts::plugins::{
    PluginCommandExecutionRouteRequest, PluginCommandExecutionRouteResponse,
    PluginExtensionRegistryRouteResponse, PluginInventoryRouteResponse,
};

pub(super) async fn list_plugins(
    State(plugins): State<PluginInventoryHandle>,
) -> Json<PluginInventoryRouteResponse> {
    Json(plugins.plugin_inventory_for_route().await)
}

pub(super) async fn list_plugin_extensions(
    State(plugins): State<PluginInventoryHandle>,
) -> Json<PluginExtensionRegistryRouteResponse> {
    Json(plugins.plugin_extension_registry_for_route().await)
}

pub(super) async fn reload_plugins(
    State(plugins): State<PluginInventoryHandle>,
) -> Result<Json<PluginInventoryRouteResponse>, StatusCode> {
    plugins
        .reload_plugins_for_route()
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

pub(super) async fn execute_plugin_command(
    State(plugins): State<PluginInventoryHandle>,
    Json(request): Json<PluginCommandExecutionRouteRequest>,
) -> Json<PluginCommandExecutionRouteResponse> {
    Json(plugins.execute_plugin_command_for_route(request).await)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ctx_core::models::PluginLoadStatus;
    use ctx_daemon::daemon::PluginInventoryRuntime;
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn plugin_handlers_reload_and_list_inventory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let plugin_dir = temp.path().join("example");
        std::fs::create_dir_all(&plugin_dir).expect("plugin dir");
        std::fs::write(
            plugin_dir.join("ctx-plugin.json"),
            serde_json::to_vec_pretty(&json!({
                "id": "example.tools",
                "name": "Example Tools",
                "version": "0.1.0",
                "entrypoints": [
                    {
                        "id": "main",
                        "command": "node",
                        "args": ["dist/index.js"],
                        "environment": {
                            "EXAMPLE_ENV": "example-value"
                        }
                    }
                ],
                "contributes": {
                    "commands": [
                        {
                            "id": "example.hello",
                            "title": "Hello",
                            "entrypoint": "main"
                        }
                    ]
                }
            }))
            .unwrap(),
        )
        .expect("write manifest");
        let handle = PluginInventoryHandle::new_for_test(Arc::new(
            PluginInventoryRuntime::new_with_roots(vec![temp.path().to_path_buf()]),
        ));

        let Json(reloaded) = reload_plugins(State(handle.clone()))
            .await
            .expect("reload plugins");
        let Json(listed) = list_plugins(State(handle)).await;

        assert_eq!(reloaded.revision, 1);
        assert_eq!(listed.revision, 1);
        assert_eq!(listed.plugins.len(), 1);
        assert_eq!(listed.plugins[0].id, "example.tools");
        assert_eq!(listed.plugins[0].status, PluginLoadStatus::Loaded);
        assert!(listed.plugins[0]
            .manifest
            .as_ref()
            .expect("manifest")
            .entrypoints[0]
            .environment
            .is_empty());
    }

    #[tokio::test]
    async fn plugin_extensions_handler_returns_active_contribution_registry() {
        let temp = tempfile::tempdir().expect("tempdir");
        let plugin_dir = temp.path().join("example");
        std::fs::create_dir_all(&plugin_dir).expect("plugin dir");
        std::fs::write(
            plugin_dir.join("ctx-plugin.json"),
            serde_json::to_vec_pretty(&json!({
                "id": "example.tools",
                "name": "Example Tools",
                "version": "0.1.0",
                "entrypoints": [
                    {
                        "id": "main",
                        "command": "node",
                        "args": ["dist/index.js"]
                    }
                ],
                "contributes": {
                    "providers": [
                        {
                            "id": "example-provider",
                            "name": "Example Provider",
                            "entrypoint": "main"
                        }
                    ],
                    "commands": [
                        {
                            "id": "example.hello",
                            "title": "Hello",
                            "entrypoint": "main"
                        }
                    ]
                }
            }))
            .unwrap(),
        )
        .expect("write manifest");
        let handle = PluginInventoryHandle::new_for_test(Arc::new(
            PluginInventoryRuntime::new_with_roots(vec![temp.path().to_path_buf()]),
        ));

        let Json(response) = list_plugin_extensions(State(handle)).await;

        assert_eq!(response.registry.revision, 1);
        assert_eq!(response.registry.providers.len(), 1);
        assert_eq!(
            response.registry.providers[0].contribution.id,
            "example-provider"
        );
        assert_eq!(response.registry.commands.len(), 1);
        assert_eq!(
            response.registry.commands[0].contribution.id,
            "example.hello"
        );
    }

    #[tokio::test]
    async fn plugin_command_handler_executes_process_entrypoint() {
        let temp = tempfile::tempdir().expect("tempdir");
        let plugin_dir = temp.path().join("example");
        std::fs::create_dir_all(&plugin_dir).expect("plugin dir");
        std::fs::write(
            plugin_dir.join("ctx-plugin.json"),
            serde_json::to_vec_pretty(&json!({
                "id": "example.tools",
                "name": "Example Tools",
                "version": "0.1.0",
                "entrypoints": [
                    {
                        "id": "main",
                        "command": "sh",
                        "args": ["-c", "cat >/dev/null; printf '{\"message\":\"expanded draft prompt\"}'"]
                    }
                ],
                "contributes": {
                    "commands": [
                        {
                            "id": "example.hello",
                            "title": "Hello",
                            "entrypoint": "main"
                        }
                    ]
                }
            }))
            .unwrap(),
        )
        .expect("write manifest");
        let handle = PluginInventoryHandle::new_for_test(Arc::new(
            PluginInventoryRuntime::new_with_roots(vec![temp.path().to_path_buf()]),
        ));

        let Json(response) = execute_plugin_command(
            State(handle),
            Json(PluginCommandExecutionRouteRequest {
                plugin_id: "example.tools".to_string(),
                command_id: "example.hello".to_string(),
                input: Some("draft prompt".to_string()),
                workspace_id: Some("workspace-1".to_string()),
                task_id: None,
                session_id: None,
            }),
        )
        .await;

        assert_eq!(
            response.status,
            ctx_route_contracts::plugins::PluginCommandExecutionStatus::Completed
        );
        assert_eq!(response.message.as_deref(), Some("expanded draft prompt"));
        assert_eq!(response.exit_code, Some(0));
    }
}
