use ctx_core::models::{PluginExtensionRegistry, PluginInventoryItem};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
pub struct PluginInventoryRouteResponse {
    pub revision: i64,
    pub roots: Vec<String>,
    pub plugins: Vec<PluginInventoryItem>,
}

impl PluginInventoryRouteResponse {
    pub fn new(revision: i64, roots: Vec<String>, plugins: Vec<PluginInventoryItem>) -> Self {
        Self {
            revision,
            roots,
            plugins: plugins
                .into_iter()
                .map(redact_plugin_inventory_item_for_route)
                .collect(),
        }
    }
}

fn redact_plugin_inventory_item_for_route(mut item: PluginInventoryItem) -> PluginInventoryItem {
    if let Some(manifest) = item.manifest.as_mut() {
        for entrypoint in &mut manifest.entrypoints {
            entrypoint.environment.clear();
        }
    }
    item
}

#[derive(Debug, Clone, Serialize)]
pub struct PluginExtensionRegistryRouteResponse {
    pub registry: PluginExtensionRegistry,
}

impl PluginExtensionRegistryRouteResponse {
    pub fn new(registry: PluginExtensionRegistry) -> Self {
        Self { registry }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct PluginCommandExecutionRouteRequest {
    pub plugin_id: String,
    pub command_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct PluginCommandInvocationPayload {
    pub schema_version: i64,
    pub plugin_id: String,
    pub command_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

impl PluginCommandInvocationPayload {
    pub fn from_request(request: &PluginCommandExecutionRouteRequest) -> Self {
        Self {
            schema_version: 1,
            plugin_id: request.plugin_id.clone(),
            command_id: request.command_id.clone(),
            input: request.input.clone(),
            workspace_id: request.workspace_id.clone(),
            task_id: request.task_id.clone(),
            session_id: request.session_id.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PluginCommandExecutionStatus {
    Completed,
    Failed,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct PluginCommandExecutionRouteResponse {
    pub plugin_id: String,
    pub command_id: String,
    pub status: PluginCommandExecutionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stdout: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stderr: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

impl PluginCommandExecutionRouteResponse {
    pub fn completed(
        plugin_id: String,
        command_id: String,
        message: Option<String>,
        stdout: String,
        stderr: String,
        exit_code: Option<i32>,
    ) -> Self {
        Self {
            plugin_id,
            command_id,
            status: PluginCommandExecutionStatus::Completed,
            message,
            error: None,
            stdout,
            stderr,
            exit_code,
        }
    }

    pub fn failed(
        plugin_id: String,
        command_id: String,
        error: impl Into<String>,
        stdout: String,
        stderr: String,
        exit_code: Option<i32>,
    ) -> Self {
        Self {
            plugin_id,
            command_id,
            status: PluginCommandExecutionStatus::Failed,
            message: None,
            error: Some(error.into()),
            stdout,
            stderr,
            exit_code,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ctx_core::models::{
        PluginCommandContribution, PluginCompatibility, PluginContributionRegistration,
        PluginContributions, PluginEnablement, PluginEntrypoint, PluginEntrypointKind,
        PluginExtensionRegistry, PluginInventoryItem, PluginLoadStatus, PluginManifest,
    };
    use serde_json::json;

    use super::*;

    #[test]
    fn plugin_extension_registry_route_response_preserves_wire_shape() {
        let registry = PluginExtensionRegistry {
            revision: 9,
            commands: vec![PluginContributionRegistration {
                plugin_id: "example.tools".to_string(),
                plugin_name: "Example Tools".to_string(),
                plugin_version: "0.1.0".to_string(),
                plugin_path: "/plugins/example/ctx-plugin.json".to_string(),
                plugin_revision: Some("abc123".to_string()),
                contribution: PluginCommandContribution {
                    id: "example.hello".to_string(),
                    title: "Hello".to_string(),
                    description: None,
                    category: Some("Example".to_string()),
                    entrypoint: Some("main".to_string()),
                },
            }],
            ..PluginExtensionRegistry::default()
        };

        assert_eq!(
            serde_json::to_value(PluginExtensionRegistryRouteResponse::new(registry)).unwrap(),
            json!({
                "registry": {
                    "revision": 9,
                    "commands": [
                        {
                            "plugin_id": "example.tools",
                            "plugin_name": "Example Tools",
                            "plugin_version": "0.1.0",
                            "plugin_path": "/plugins/example/ctx-plugin.json",
                            "plugin_revision": "abc123",
                            "contribution": {
                                "id": "example.hello",
                                "title": "Hello",
                                "category": "Example",
                                "entrypoint": "main"
                            }
                        }
                    ]
                }
            })
        );
    }

    #[test]
    fn plugin_inventory_route_response_redacts_entrypoint_environment() {
        let response = PluginInventoryRouteResponse::new(
            1,
            vec!["/plugins".to_string()],
            vec![PluginInventoryItem {
                id: "example.tools".to_string(),
                name: "Example Tools".to_string(),
                version: "0.1.0".to_string(),
                enabled: PluginEnablement::Enabled,
                status: PluginLoadStatus::Loaded,
                path: "/plugins/example/ctx-plugin.json".to_string(),
                diagnostics: Vec::new(),
                last_loaded_at: None,
                revision: None,
                manifest: Some(PluginManifest {
                    schema_version: 1,
                    id: "example.tools".to_string(),
                    name: "Example Tools".to_string(),
                    version: "0.1.0".to_string(),
                    description: None,
                    entrypoints: vec![PluginEntrypoint {
                        id: "main".to_string(),
                        kind: PluginEntrypointKind::Process,
                        command: "node".to_string(),
                        args: Vec::new(),
                        cwd: None,
                        environment: BTreeMap::from([(
                            "EXAMPLE_ENV".to_string(),
                            "example-value".to_string(),
                        )]),
                    }],
                    contributes: PluginContributions::default(),
                    compatibility: PluginCompatibility::default(),
                }),
            }],
        );

        assert_eq!(
            serde_json::to_value(response).unwrap(),
            json!({
                "revision": 1,
                "roots": ["/plugins"],
                "plugins": [
                    {
                        "id": "example.tools",
                        "name": "Example Tools",
                        "version": "0.1.0",
                        "enabled": "enabled",
                        "status": "loaded",
                        "path": "/plugins/example/ctx-plugin.json",
                        "manifest": {
                            "schema_version": 1,
                            "id": "example.tools",
                            "name": "Example Tools",
                            "version": "0.1.0",
                            "entrypoints": [
                                {
                                    "id": "main",
                                    "kind": "process",
                                    "command": "node"
                                }
                            ]
                        }
                    }
                ]
            })
        );
    }

    #[test]
    fn plugin_command_execution_route_response_preserves_wire_shape() {
        let response = PluginCommandExecutionRouteResponse::completed(
            "example.tools".to_string(),
            "example.hello".to_string(),
            Some("Hello from plugin".to_string()),
            "{\"message\":\"Hello from plugin\"}\n".to_string(),
            String::new(),
            Some(0),
        );

        assert_eq!(
            serde_json::to_value(response).unwrap(),
            json!({
                "plugin_id": "example.tools",
                "command_id": "example.hello",
                "status": "completed",
                "message": "Hello from plugin",
                "stdout": "{\"message\":\"Hello from plugin\"}\n",
                "exit_code": 0
            })
        );
    }
}
