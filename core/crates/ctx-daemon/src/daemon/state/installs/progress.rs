use super::*;

impl DaemonState {
    pub(crate) fn emit_provider_install_ops_events(
        &self,
        events: Vec<ctx_provider_runtime::provider_install_tracker::ProviderInstallOpsEvent>,
    ) {
        for event in events {
            self.emit_provider_install_ops_event(event);
        }
    }

    pub(crate) fn emit_provider_install_ops_event(
        &self,
        event: ctx_provider_runtime::provider_install_tracker::ProviderInstallOpsEvent,
    ) {
        let mut ops_event = OpsEvent::new(event.level, event.name);
        ops_event.provider_id = Some(event.provider_id);
        let mut meta = serde_json::Map::new();
        meta.insert(
            "install_id".to_string(),
            serde_json::Value::String(event.install_id.to_string()),
        );
        if let Some(target) = event.target {
            meta.insert(
                "target".to_string(),
                serde_json::Value::String(target.as_str().to_string()),
            );
        }
        if let Some(state) = event.state {
            meta.insert(
                "state".to_string(),
                serde_json::Value::String(
                    match state {
                        InstallStateKind::Running => "running",
                        InstallStateKind::Succeeded => "succeeded",
                        InstallStateKind::Failed => "failed",
                        InstallStateKind::Cancelled => "cancelled",
                    }
                    .to_string(),
                ),
            );
        }
        if let Some(error) = event.error {
            meta.insert("error".to_string(), serde_json::Value::String(error));
        }
        if let Some(error_code) = event
            .error_code
            .and_then(|value| serde_json::to_value(value).ok())
        {
            meta.insert("error_code".to_string(), error_code);
        }
        if let Some(ok) = event.ok {
            meta.insert("ok".to_string(), serde_json::Value::Bool(ok));
        }
        ops_event.meta = Some(serde_json::Value::Object(meta));
        self.telemetry.ops_events.emit(ops_event);
    }
}
