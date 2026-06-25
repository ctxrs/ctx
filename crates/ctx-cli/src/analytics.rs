use std::{env, path::Path, time::Duration};

use anyhow::Result;
use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use crate::{config::AppConfig, identity, net};

#[derive(Debug, Clone)]
pub struct AnalyticsEvent<'a> {
    pub action: &'a str,
    pub json_output: bool,
    pub success: bool,
    pub duration: Duration,
}

pub fn send_cli_event(data_root: &Path, config: &AppConfig, event: AnalyticsEvent<'_>) {
    if !config.analytics.enabled || env::var_os("CTX_ANALYTICS_DRY_RUN").is_some() {
        return;
    }
    if let Err(err) = send_cli_event_inner(data_root, config, event) {
        if env::var_os("CTX_ANALYTICS_DEBUG").is_some() {
            eprintln!("ctx analytics delivery failed: {err:#}");
        }
    }
}

fn send_cli_event_inner(
    data_root: &Path,
    config: &AppConfig,
    event: AnalyticsEvent<'_>,
) -> Result<()> {
    let install_id = identity::install_id(data_root)?;
    let status = if event.success { "ok" } else { "error" };
    let duration_ms = event.duration.as_millis().min(i64::MAX as u128) as i64;
    let payload = json!({
        "broker_install_id": install_id,
        "broker_runtime": "cli",
        "broker_app_version": env!("CARGO_PKG_VERSION"),
        "broker_os": std::env::consts::OS,
        "broker_arch": std::env::consts::ARCH,
        "events": [{
            "event_id": Uuid::now_v7().to_string(),
            "event_name": "cli_invocation",
            "event_version": 1,
            "occurred_at": Utc::now(),
            "plane": "product",
            "delivery": "remote",
            "origin_runtime": "cli",
            "origin_install_id": install_id,
            "app_version": env!("CARGO_PKG_VERSION"),
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "surface": "cli",
            "source": "ctx-cli",
            "duration_ms": duration_ms,
            "duration_bucket": duration_bucket(event.duration),
            "status": status,
            "success": event.success,
            "properties": {
                "action": event.action,
                "json_output": event.json_output,
                "analytics_client": "ctx-cli"
            }
        }]
    });
    let body = serde_json::to_vec(&payload)?;
    net::post_json(&config.analytics.endpoint, &body)
}

fn duration_bucket(duration: Duration) -> &'static str {
    let ms = duration.as_millis();
    match ms {
        0..=99 => "lt_100ms",
        100..=999 => "lt_1s",
        1_000..=4_999 => "lt_5s",
        5_000..=29_999 => "lt_30s",
        _ => "gte_30s",
    }
}
