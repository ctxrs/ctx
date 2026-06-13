use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use ctx_observability::telemetry::{
    TelemetryDelivery, TelemetryEvent, TelemetryOriginRuntime, TelemetryPlane,
};
use serde::Deserialize;
use serde_json::{Map, Value};

use super::super::sanitizer::{normalize_optional_string, sanitize_semantic_properties};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SemanticTelemetryEventReq {
    event_id: String,
    event_name: String,
    event_version: u32,
    occurred_at: DateTime<Utc>,
    plane: TelemetryPlane,
    delivery: TelemetryDelivery,
    origin_runtime: TelemetryOriginRuntime,
    origin_install_id: String,
    app_version: String,
    os: String,
    arch: String,
    #[serde(default)]
    surface: Option<String>,
    #[serde(default)]
    env_target: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    properties: Option<Map<String, Value>>,
}

impl SemanticTelemetryEventReq {
    pub(super) fn into_event(self) -> Result<TelemetryEvent, StatusCode> {
        let event_id = self.event_id.trim().to_string();
        let event_name = self.event_name.trim().to_string();
        let origin_install_id = self.origin_install_id.trim().to_string();
        let app_version = self.app_version.trim().to_string();
        let os = self.os.trim().to_string();
        let arch = self.arch.trim().to_string();
        if event_id.is_empty()
            || event_name.is_empty()
            || self.event_version == 0
            || origin_install_id.is_empty()
            || app_version.is_empty()
            || os.is_empty()
            || arch.is_empty()
        {
            return Err(StatusCode::BAD_REQUEST);
        }
        Ok(TelemetryEvent {
            event_id,
            event_name,
            event_version: self.event_version,
            occurred_at: self.occurred_at,
            plane: self.plane,
            delivery: self.delivery,
            origin_runtime: self.origin_runtime,
            origin_install_id: Some(origin_install_id),
            app_version,
            os,
            arch,
            surface: normalize_optional_string(self.surface),
            env_target: normalize_optional_string(self.env_target),
            source: normalize_optional_string(self.source),
            properties: sanitize_semantic_properties(self.properties.unwrap_or_default()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn semantic_event_requires_non_empty_core_fields() {
        let event = SemanticTelemetryEventReq {
            event_id: "   ".to_string(),
            event_name: "renderer_backlog_spike".to_string(),
            event_version: 1,
            occurred_at: Utc.with_ymd_and_hms(2026, 4, 21, 12, 0, 0).unwrap(),
            plane: TelemetryPlane::Incident,
            delivery: TelemetryDelivery::Remote,
            origin_runtime: TelemetryOriginRuntime::Web,
            origin_install_id: "install-1".to_string(),
            app_version: "1.2.3".to_string(),
            os: "macos".to_string(),
            arch: "arm64".to_string(),
            surface: Some("desktop".to_string()),
            env_target: None,
            source: None,
            properties: None,
        };

        assert_eq!(event.into_event().unwrap_err(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn semantic_event_preserves_delivery_and_sanitizes_optional_fields() {
        let event = SemanticTelemetryEventReq {
            event_id: " event-1 ".to_string(),
            event_name: " worker_patch_apply ".to_string(),
            event_version: 2,
            occurred_at: Utc.with_ymd_and_hms(2026, 4, 21, 12, 0, 0).unwrap(),
            plane: TelemetryPlane::Incident,
            delivery: TelemetryDelivery::LocalOnly,
            origin_runtime: TelemetryOriginRuntime::Desktop,
            origin_install_id: " install-1 ".to_string(),
            app_version: " 1.2.3 ".to_string(),
            os: " macos ".to_string(),
            arch: " arm64 ".to_string(),
            surface: Some(" desktop ".to_string()),
            env_target: Some("   ".to_string()),
            source: Some(" worker_patch ".to_string()),
            properties: None,
        };

        let event = event.into_event().expect("valid semantic event");
        assert_eq!(event.event_id, "event-1");
        assert_eq!(event.delivery, TelemetryDelivery::LocalOnly);
        assert_eq!(event.surface.as_deref(), Some("desktop"));
        assert_eq!(event.env_target, None);
        assert_eq!(event.source.as_deref(), Some("worker_patch"));
    }
}
