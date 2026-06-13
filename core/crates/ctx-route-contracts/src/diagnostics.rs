use serde::Serialize;

use crate::health::DaemonHealthSnapshot;

#[derive(Debug, Serialize)]
pub struct DaemonDiagnosticsSnapshot {
    pub daemon: DaemonHealthSnapshot,
    pub platform: serde_json::Value,
    pub logs: serde_json::Value,
    pub execution: serde_json::Value,
    pub providers: Vec<serde_json::Value>,
    pub managed_installs: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::{DaemonHealthSnapshot, HealthCompatibility};
    use serde_json::json;

    #[test]
    fn diagnostics_snapshot_preserves_nested_json_wire_shape() {
        let snapshot = DaemonDiagnosticsSnapshot {
            daemon: DaemonHealthSnapshot {
                version: "1.2.3".to_string(),
                daemon_version: "1.2.3".to_string(),
                pid: None,
                data_root: None,
                daemon_url: None,
                auth_required: false,
                open_file_limit: None,
                storage: None,
                compatibility: HealthCompatibility {
                    desktop_exact_version: "1.2.3".to_string(),
                    desktop_build_id: "build-123".to_string(),
                    desktop_dev_instance_id: String::new(),
                    protocol_compatibility_token: String::new(),
                    mobile_api_min: 1,
                    mobile_api_max: 1,
                },
            },
            platform: json!({"os": "macos", "arch": "aarch64"}),
            logs: json!({"dir": "/tmp/logs", "files": ["ctx.log"]}),
            execution: json!({"startup_prewarm": {"ready": true}}),
            providers: vec![json!({"id": "fake", "ready": true})],
            managed_installs: json!({"title_generation": {"installed": true}}),
        };

        assert_eq!(
            serde_json::to_value(snapshot).unwrap(),
            json!({
                "daemon": {
                    "version": "1.2.3",
                    "daemon_version": "1.2.3",
                    "auth_required": false,
                    "compatibility": {
                        "desktop_exact_version": "1.2.3",
                        "desktop_build_id": "build-123",
                        "desktop_dev_instance_id": "",
                        "protocol_compatibility_token": "",
                        "mobile_api_min": 1,
                        "mobile_api_max": 1,
                    }
                },
                "platform": {"os": "macos", "arch": "aarch64"},
                "logs": {"dir": "/tmp/logs", "files": ["ctx.log"]},
                "execution": {"startup_prewarm": {"ready": true}},
                "providers": [{"id": "fake", "ready": true}],
                "managed_installs": {"title_generation": {"installed": true}},
            })
        );
    }
}
