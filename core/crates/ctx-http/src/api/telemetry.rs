use super::*;
use chrono::{NaiveDate, Utc};
use ctx_daemon::daemon::TelemetryHandle;
use ctx_route_contracts::telemetry::{TelemetryExportError, TelemetryExportErrorKind};
use serde::Deserialize;

mod semantic;

pub(super) use semantic::post_semantic_telemetry;

#[derive(Debug, Deserialize)]
pub(super) struct TelemetrySummaryQuery {
    metric: Option<String>,
    run_id: Option<String>,
    window_ms: Option<u64>,
    limit: Option<u32>,
}

pub(super) async fn get_telemetry_summary(
    State(state): State<TelemetryHandle>,
    Query(q): Query<TelemetrySummaryQuery>,
) -> Result<Json<ctx_observability::perf_telemetry::PerfSummary>, StatusCode> {
    let summary = state.perf_telemetry().summary(
        q.metric.as_deref(),
        q.run_id.as_deref(),
        q.window_ms,
        q.limit.map(|v| v as usize),
    );
    Ok(Json(summary))
}

#[derive(Debug, Deserialize)]
pub(super) struct TelemetryExportQuery {
    date: Option<String>,
}

fn normalize_export_date(raw: Option<String>) -> Result<String, StatusCode> {
    let date = match raw {
        Some(raw) => NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d")
            .map_err(|_| StatusCode::BAD_REQUEST)?,
        None => Utc::now().date_naive(),
    };
    Ok(date.format("%Y-%m-%d").to_string())
}

fn telemetry_export_status(error: TelemetryExportError) -> StatusCode {
    match error.kind() {
        TelemetryExportErrorKind::NotFound => StatusCode::NOT_FOUND,
    }
}

pub(super) async fn export_telemetry(
    State(telemetry): State<TelemetryHandle>,
    Query(q): Query<TelemetryExportQuery>,
) -> Result<Response, StatusCode> {
    let date = normalize_export_date(q.date)?;
    let bytes = telemetry
        .read_perf_telemetry_export_for_date(&date)
        .await
        .map_err(telemetry_export_status)?;
    let mut resp = Response::new(Body::from(bytes));
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    Ok(resp)
}

#[derive(Debug, Deserialize)]
pub(super) struct ClientTelemetryMetric {
    name: String,
    kind: PerfMetricKind,
    unit: String,
    value: f64,
    labels: Option<HashMap<String, String>>,
    run_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ClientTelemetryBatch {
    events: Vec<ClientTelemetryMetric>,
}

pub(super) async fn post_client_telemetry(
    State(state): State<TelemetryHandle>,
    Json(batch): Json<ClientTelemetryBatch>,
) -> Result<StatusCode, StatusCode> {
    for event in batch.events {
        let mut labels = event.labels.unwrap_or_default();
        labels.insert("source".to_string(), "client".to_string());
        let metric = PerfMetric {
            name: event.name,
            kind: event.kind,
            unit: event.unit,
            value: event.value,
            labels,
        };
        state
            .perf_telemetry()
            .record_metric(metric, event.run_id, None, None)
            .await;
    }
    Ok(StatusCode::NO_CONTENT)
}
