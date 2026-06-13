use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct HealthCompatibility {
    pub desktop_exact_version: String,
    pub desktop_build_id: String,
    pub desktop_dev_instance_id: String,
    pub protocol_compatibility_token: String,
    pub mobile_api_min: i64,
    pub mobile_api_max: i64,
}

#[derive(Debug, Serialize)]
pub struct DaemonHealthSnapshot {
    pub version: String,
    pub daemon_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daemon_url: Option<String>,
    pub auth_required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_file_limit: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage: Option<serde_json::Value>,
    pub compatibility: HealthCompatibility,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn compatibility() -> HealthCompatibility {
        HealthCompatibility {
            desktop_exact_version: "1.2.3".to_string(),
            desktop_build_id: "build-123".to_string(),
            desktop_dev_instance_id: "compat-token".to_string(),
            protocol_compatibility_token: "compat-token".to_string(),
            mobile_api_min: 1,
            mobile_api_max: 1,
        }
    }

    #[test]
    fn health_snapshot_omits_sensitive_optional_fields_when_absent() {
        let snapshot = DaemonHealthSnapshot {
            version: "1.2.3".to_string(),
            daemon_version: "1.2.3".to_string(),
            pid: None,
            data_root: None,
            daemon_url: None,
            auth_required: true,
            open_file_limit: None,
            storage: None,
            compatibility: compatibility(),
        };

        assert_eq!(
            serde_json::to_value(snapshot).unwrap(),
            json!({
                "version": "1.2.3",
                "daemon_version": "1.2.3",
                "auth_required": true,
                "compatibility": {
                    "desktop_exact_version": "1.2.3",
                    "desktop_build_id": "build-123",
                    "desktop_dev_instance_id": "compat-token",
                    "protocol_compatibility_token": "compat-token",
                    "mobile_api_min": 1,
                    "mobile_api_max": 1,
                }
            })
        );
    }

    #[test]
    fn health_snapshot_preserves_nested_runtime_payloads() {
        let snapshot = DaemonHealthSnapshot {
            version: "1.2.3".to_string(),
            daemon_version: "1.2.3".to_string(),
            pid: Some(123),
            data_root: Some("/tmp/ctx".to_string()),
            daemon_url: Some("http://127.0.0.1:0".to_string()),
            auth_required: false,
            open_file_limit: Some(json!({"soft": 65535, "hard": 65535})),
            storage: Some(json!({
                "level": "normal",
                "warning_threshold_bytes": 2147483648u64,
                "emergency_threshold_bytes": 1073741824u64,
                "reserve_bytes": 536870912u64,
                "reserve_file_active": false,
                "active": null,
                "updated_at": "2026-05-22T00:00:00Z",
            })),
            compatibility: compatibility(),
        };

        assert_eq!(
            serde_json::to_value(snapshot).unwrap(),
            json!({
                "version": "1.2.3",
                "daemon_version": "1.2.3",
                "pid": 123,
                "data_root": "/tmp/ctx",
                "daemon_url": "http://127.0.0.1:0",
                "auth_required": false,
                "open_file_limit": {"soft": 65535, "hard": 65535},
                "storage": {
                    "level": "normal",
                    "warning_threshold_bytes": 2147483648u64,
                    "emergency_threshold_bytes": 1073741824u64,
                    "reserve_bytes": 536870912u64,
                    "reserve_file_active": false,
                    "active": null,
                    "updated_at": "2026-05-22T00:00:00Z",
                },
                "compatibility": {
                    "desktop_exact_version": "1.2.3",
                    "desktop_build_id": "build-123",
                    "desktop_dev_instance_id": "compat-token",
                    "protocol_compatibility_token": "compat-token",
                    "mobile_api_min": 1,
                    "mobile_api_max": 1,
                }
            })
        );
    }
}
