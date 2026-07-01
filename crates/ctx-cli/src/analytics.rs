use std::{env, path::Path, time::Duration};

use anyhow::Result;
use ctx_history_core::utc_now;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::{config::AppConfig, identity, net};

pub type AnalyticsProperties = Map<String, Value>;

#[derive(Debug, Clone)]
pub struct AnalyticsEvent<'a> {
    pub action: &'a str,
    pub json_output: bool,
    pub success: bool,
    pub duration: Duration,
    pub properties: AnalyticsProperties,
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
    let mut properties = event.properties;
    properties.insert("action".to_owned(), Value::String(event.action.to_owned()));
    properties.insert("json_output".to_owned(), Value::Bool(event.json_output));
    properties.insert(
        "analytics_client".to_owned(),
        Value::String("ctx-cli".to_owned()),
    );
    if !event.success {
        properties.insert(
            "failure_kind".to_owned(),
            Value::String("command_error".to_owned()),
        );
    }
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
            "occurred_at": utc_now(),
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
            "properties": properties
        }]
    });
    let body = serde_json::to_vec(&payload)?;
    net::post_json(&config.analytics.endpoint, &body)
}

pub fn empty_properties() -> AnalyticsProperties {
    Map::new()
}

pub fn insert_str(properties: &mut AnalyticsProperties, key: &str, value: impl Into<String>) {
    properties.insert(key.to_owned(), Value::String(value.into()));
}

pub fn insert_bool(properties: &mut AnalyticsProperties, key: &str, value: bool) {
    properties.insert(key.to_owned(), Value::Bool(value));
}

pub fn insert_count_bucket(properties: &mut AnalyticsProperties, key: &str, count: u64) {
    insert_str(properties, key, count_bucket(count));
}

pub fn insert_bytes_bucket(properties: &mut AnalyticsProperties, key: &str, bytes: u64) {
    insert_str(properties, key, bytes_bucket(bytes));
}

pub fn count_bucket(count: u64) -> &'static str {
    match count {
        0 => "0",
        1 => "1",
        2..=5 => "2-5",
        6..=20 => "6-20",
        21..=100 => "21-100",
        101..=1_000 => "101-1k",
        _ => "1k+",
    }
}

pub fn bytes_bucket(bytes: u64) -> &'static str {
    match bytes {
        0 => "0",
        1..=102_399 => "lt_100kb",
        102_400..=1_048_575 => "100kb-1mb",
        1_048_576..=10_485_759 => "1mb-10mb",
        10_485_760..=104_857_599 => "10mb-100mb",
        104_857_600..=1_073_741_823 => "100mb-1gb",
        _ => "1gb+",
    }
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
