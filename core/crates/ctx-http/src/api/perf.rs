use super::*;
use ctx_daemon::daemon::TelemetryHandle;

pub(in crate::api) async fn perf_middleware(
    State(state): State<TelemetryHandle>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let start = Instant::now();
    let method = req.method().to_string();
    let run_id = req
        .headers()
        .get("x-ctx-run-id")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_string());
    let endpoint = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_string())
        .unwrap_or_else(|| req.uri().path().to_string());
    let parent = state.perf_telemetry().extract_trace_context(req.headers());
    let span = state.perf_telemetry().start_span(
        "http_request",
        SpanKind::Server,
        Some(parent),
        vec![
            KeyValue::new("http.method", method.clone()),
            KeyValue::new("http.route", endpoint.clone()),
        ],
    );
    let response = next.run(req).await;
    let status = response.status().as_u16();
    let duration_ms = start.elapsed().as_millis() as u64;
    let success = status < 500;
    let mut labels = HashMap::new();
    labels.insert("endpoint".to_string(), endpoint);
    labels.insert("method".to_string(), method);
    labels.insert("status".to_string(), status.to_string());
    labels.insert("success".to_string(), success.to_string());
    labels.insert("source".to_string(), "daemon".to_string());
    let (trace_id, span_id) = state.perf_telemetry().finish_span(
        span,
        Some(status.to_string()),
        Some(success),
        vec![KeyValue::new("http.status_code", status as i64)],
    );
    let metric = PerfMetric {
        name: "http.request.duration_ms".to_string(),
        kind: PerfMetricKind::Histogram,
        unit: "ms".to_string(),
        value: duration_ms as f64,
        labels,
    };
    state
        .perf_telemetry()
        .record_metric(metric, run_id, trace_id, span_id)
        .await;
    response
}
