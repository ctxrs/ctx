use anyhow::Result;
use reqwest::Method;
use serde_json::Value;
use url::form_urlencoded;

use crate::client::Client;
use crate::types::{ClientTelemetryBatch, TelemetrySummaryParams};

impl Client {
    pub async fn post_client_telemetry(&self, batch: &ClientTelemetryBatch) -> Result<()> {
        self.request_empty(Method::POST, "/api/telemetry/client", Some(batch))
            .await
    }

    pub async fn get_telemetry_summary(&self, params: &TelemetrySummaryParams) -> Result<Value> {
        let mut path = "/api/telemetry/summary".to_string();
        let mut search = Vec::new();
        if let Some(metric) = &params.metric {
            let metric = form_urlencoded::byte_serialize(metric.as_bytes()).collect::<String>();
            search.push(format!("metric={metric}"));
        }
        if let Some(run_id) = &params.run_id {
            let run_id = form_urlencoded::byte_serialize(run_id.as_bytes()).collect::<String>();
            search.push(format!("run_id={run_id}"));
        }
        if let Some(window_ms) = params.window_ms {
            search.push(format!("window_ms={window_ms}"));
        }
        if let Some(limit) = params.limit {
            search.push(format!("limit={limit}"));
        }
        if !search.is_empty() {
            path.push('?');
            path.push_str(&search.join("&"));
        }
        self.request_json(Method::GET, &path, None::<&()>).await
    }
}
