use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub mod logs {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    use anyhow::{Context, Result};
    use tokio::io::AsyncWriteExt;

    pub fn redact_sensitive(value: &str) -> String {
        let mut out = value.to_string();
        for key in [
            "token",
            "secret",
            "password",
            "api_key",
            "apikey",
            "authorization",
            "bearer",
        ] {
            out = redact_key_like(&out, key);
        }
        out
    }

    fn redact_key_like(input: &str, key: &str) -> String {
        input
            .split_whitespace()
            .map(|part| {
                let lower = part.to_ascii_lowercase();
                if lower.starts_with(&format!("{key}=")) || lower.starts_with(&format!("{key}:")) {
                    format!("{key}=<redacted>")
                } else {
                    part.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub fn logs_dir(data_root: &Path) -> PathBuf {
        data_root.join("logs")
    }

    pub async fn open_logs_folder(data_root: &Path) -> Result<()> {
        let dir = logs_dir(data_root);
        tokio::fs::create_dir_all(&dir)
            .await
            .with_context(|| format!("creating logs directory {}", dir.display()))?;
        Ok(())
    }

    pub async fn append_desktop_log_line(data_root: &Path, line: &str) -> Result<()> {
        let dir = logs_dir(data_root);
        tokio::fs::create_dir_all(&dir)
            .await
            .with_context(|| format!("creating logs directory {}", dir.display()))?;
        let path = dir.join("desktop.log");
        let mut line = line.to_string();
        if !line.ends_with('\n') {
            line.push('\n');
        }
        tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
            .with_context(|| format!("opening {}", path.display()))?
            .write_all(line.as_bytes())
            .await
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    pub async fn list_log_files(data_root: &Path) -> Vec<serde_json::Value> {
        let dir = logs_dir(data_root);
        let mut out = Vec::new();
        let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
            return out;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let Ok(metadata) = entry.metadata().await else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            out.push(serde_json::json!({
                "name": entry.file_name().to_string_lossy(),
                "bytes": metadata.len(),
            }));
        }
        out
    }

    pub fn default_ctx_logs_dir() -> Result<PathBuf> {
        let project_dirs = directories::ProjectDirs::from("dev", "ctx", "ctx")
            .context("failed to resolve ctx data directory")?;
        Ok(project_dirs.data_dir().join("logs"))
    }

    #[derive(Debug, Clone)]
    pub struct DaemonLogConfig {
        pub stdout_enabled: bool,
    }

    impl DaemonLogConfig {
        pub fn from_env() -> Self {
            let stdout_enabled = std::env::var("CTX_DAEMON_LOG_STDOUT")
                .ok()
                .as_deref()
                .and_then(ctx_core::boolish::parse_boolish)
                .unwrap_or(false);
            Self { stdout_enabled }
        }
    }

    pub fn prepare_daemon_log_file_for_today_sync(logs_dir: &Path) {
        if let Err(error) = std::fs::create_dir_all(logs_dir) {
            tracing::warn!(
                path = %logs_dir.display(),
                "failed to create daemon logs directory: {error:#}"
            );
        }
    }

    pub fn spawn_daemon_log_maintenance(
        _logs_dir: PathBuf,
        _config: DaemonLogConfig,
        _file_blocked: Arc<AtomicBool>,
    ) {
    }
}

pub mod perf_telemetry {
    use super::*;
    use opentelemetry::trace::SpanKind;
    use opentelemetry::KeyValue;

    const MAX_METRICS: usize = 2048;

    #[derive(Debug, Clone, Copy, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum PerfMetricKind {
        Counter,
        Gauge,
        Histogram,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct PerfMetric {
        pub name: String,
        pub kind: PerfMetricKind,
        pub unit: String,
        pub value: f64,
        #[serde(default)]
        pub labels: HashMap<String, String>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct PerfMetricRecord {
        pub metric: PerfMetric,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub run_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub trace_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub span_id: Option<String>,
        pub recorded_at: DateTime<Utc>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, Default)]
    pub struct PerfSummary {
        pub metrics: Vec<PerfMetricRecord>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, Default)]
    pub struct PerfTelemetryStats {
        pub buffered_metric_count: usize,
        pub dropped_metric_count: usize,
    }

    #[derive(Debug, Default)]
    struct PerfTelemetryInner {
        metrics: Vec<PerfMetricRecord>,
        dropped: usize,
    }

    #[derive(Debug, Clone)]
    pub struct PerfTelemetry {
        data_root: PathBuf,
        inner: Arc<Mutex<PerfTelemetryInner>>,
        remote_enabled: Arc<Mutex<bool>>,
    }

    #[derive(Debug, Clone, Default)]
    pub struct PerfTraceContext {
        trace_id: Option<String>,
        span_id: Option<String>,
    }

    #[derive(Debug, Clone)]
    pub struct PerfSpan {
        trace_id: Option<String>,
        span_id: Option<String>,
    }

    impl PerfTelemetry {
        pub fn new(data_root: PathBuf) -> Self {
            Self {
                data_root,
                inner: Arc::new(Mutex::new(PerfTelemetryInner::default())),
                remote_enabled: Arc::new(Mutex::new(true)),
            }
        }

        pub async fn record_metric(
            &self,
            metric: PerfMetric,
            run_id: Option<String>,
            trace_id: Option<String>,
            span_id: Option<String>,
        ) {
            let record = PerfMetricRecord {
                metric,
                run_id,
                trace_id,
                span_id,
                recorded_at: Utc::now(),
            };
            let mut inner = self.inner.lock().expect("perf telemetry mutex poisoned");
            if inner.metrics.len() >= MAX_METRICS {
                inner.metrics.remove(0);
                inner.dropped += 1;
            }
            inner.metrics.push(record);
        }

        pub fn export_remote_metric(&self, metric: PerfMetric) {
            let this = self.clone();
            tokio::spawn(async move {
                this.record_metric(metric, None, None, None).await;
            });
        }

        pub fn summary(
            &self,
            metric: Option<&str>,
            run_id: Option<&str>,
            _window_ms: Option<u64>,
            limit: Option<usize>,
        ) -> PerfSummary {
            let inner = self.inner.lock().expect("perf telemetry mutex poisoned");
            let mut metrics = inner
                .metrics
                .iter()
                .filter(|record| metric.is_none_or(|name| record.metric.name == name))
                .filter(|record| run_id.is_none_or(|id| record.run_id.as_deref() == Some(id)))
                .cloned()
                .collect::<Vec<_>>();
            let limit = limit.unwrap_or(metrics.len());
            if metrics.len() > limit {
                metrics = metrics.split_off(metrics.len() - limit);
            }
            PerfSummary { metrics }
        }

        pub fn stats(&self) -> PerfTelemetryStats {
            let inner = self.inner.lock().expect("perf telemetry mutex poisoned");
            PerfTelemetryStats {
                buffered_metric_count: inner.metrics.len(),
                dropped_metric_count: inner.dropped,
            }
        }

        pub fn extract_trace_context(&self, headers: &http::HeaderMap) -> PerfTraceContext {
            let traceparent = headers
                .get("traceparent")
                .and_then(|value| value.to_str().ok());
            let mut context = PerfTraceContext::default();
            if let Some(traceparent) = traceparent {
                let mut parts = traceparent.split('-');
                let _version = parts.next();
                context.trace_id = parts.next().map(ToOwned::to_owned);
                context.span_id = parts.next().map(ToOwned::to_owned);
            }
            context
        }

        pub fn start_span(
            &self,
            name: &'static str,
            _kind: SpanKind,
            parent: Option<PerfTraceContext>,
            _attributes: Vec<KeyValue>,
        ) -> PerfSpan {
            let trace_id = parent
                .and_then(|parent| parent.trace_id)
                .unwrap_or_else(next_trace_id);
            tracing::trace!(span_name = name, trace_id = %trace_id, "perf span started");
            PerfSpan {
                trace_id: Some(trace_id),
                span_id: Some(next_span_id()),
            }
        }

        pub fn finish_span(
            &self,
            span: PerfSpan,
            _status: Option<String>,
            _success: Option<bool>,
            _attributes: Vec<KeyValue>,
        ) -> (Option<String>, Option<String>) {
            (span.trace_id, span.span_id)
        }

        pub fn data_root(&self) -> &Path {
            &self.data_root
        }

        pub async fn update_remote_enabled(&self, enabled: bool) {
            *self
                .remote_enabled
                .lock()
                .expect("perf telemetry mutex poisoned") = enabled;
        }
    }

    fn next_trace_id() -> String {
        format!(
            "{:032x}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        )
    }

    fn next_span_id() -> String {
        format!(
            "{:016x}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default() & i64::MAX
        )
    }

    pub fn perf_log_path_for_date(data_root: &Path, date: &str) -> PathBuf {
        data_root
            .join("logs")
            .join("telemetry")
            .join(format!("perf-{date}.jsonl"))
    }
}

pub mod ops_events {
    use super::*;
    use ctx_core::ids::WorkspaceId;

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct OpsEvent {
        pub level: String,
        pub name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub task_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub workspace_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub worktree_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub provider_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub run_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub turn_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub worktree_root: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub tool_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub meta: Option<serde_json::Value>,
        pub occurred_at: DateTime<Utc>,
    }

    impl OpsEvent {
        pub fn new(level: impl Into<String>, name: impl Into<String>) -> Self {
            Self {
                level: level.into(),
                name: name.into(),
                session_id: None,
                task_id: None,
                workspace_id: None,
                worktree_id: None,
                provider_id: None,
                run_id: None,
                turn_id: None,
                cwd: None,
                worktree_root: None,
                tool_kind: None,
                meta: None,
                occurred_at: Utc::now(),
            }
        }
    }

    #[derive(Debug, Clone)]
    pub struct OpsEvents {
        data_root: PathBuf,
    }

    impl OpsEvents {
        pub fn new(data_root: PathBuf) -> Self {
            Self { data_root }
        }

        pub fn emit(&self, event: OpsEvent) {
            tracing::debug!(
                level = %event.level,
                name = %event.name,
                data_root = %self.data_root.display(),
                "ops event"
            );
        }
    }

    #[derive(Debug, Clone)]
    pub struct SubstrateLifecycleOpsEventContext {
        pub source: &'static str,
        pub workspace_id: Option<String>,
    }

    pub fn substrate_lifecycle_observed_event(
        record: &ctx_avf_linux_runtime::SubstrateLifecycleRecord,
        context: SubstrateLifecycleOpsEventContext,
    ) -> OpsEvent {
        let mut event = OpsEvent::new("info", "substrate_lifecycle_observed");
        event.workspace_id = context.workspace_id;
        event.meta = Some(serde_json::json!({
            "source": context.source,
            "record": serde_json::to_value(record).unwrap_or(serde_json::Value::Null),
        }));
        event
    }

    #[allow(dead_code)]
    fn _workspace_id(_: WorkspaceId) {}
}

pub mod telemetry {
    use super::*;

    pub type TelemetryProperties = BTreeMap<String, serde_json::Value>;

    #[derive(Debug, Clone, Copy, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum TelemetryPlane {
        Operational,
        Product,
        Billing,
        Incident,
    }

    #[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum TelemetryDelivery {
        Remote,
        LocalOnly,
    }

    #[derive(Debug, Clone, Copy, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum TelemetryOriginRuntime {
        Daemon,
        Desktop,
        Web,
        MobileShell,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct TelemetryEvent {
        pub event_id: String,
        pub event_name: String,
        pub event_version: u32,
        pub occurred_at: DateTime<Utc>,
        pub plane: TelemetryPlane,
        pub delivery: TelemetryDelivery,
        pub origin_runtime: TelemetryOriginRuntime,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub origin_install_id: Option<String>,
        pub app_version: String,
        pub os: String,
        pub arch: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub surface: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub env_target: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub source: Option<String>,
        #[serde(default)]
        pub properties: TelemetryProperties,
    }

    impl TelemetryEvent {
        pub fn daemon_incident(event_name: impl Into<String>) -> Self {
            Self::local("daemon_incident", event_name.into())
        }

        pub fn workspace_opened() -> Self {
            Self::local("workspace", "workspace_opened")
        }

        pub fn workspace_registered() -> Self {
            Self::local("workspace", "workspace_registered")
        }

        pub fn session_started(
            provider_id: String,
            model_id: String,
            execution_environment: Option<String>,
            session_root_kind: Option<String>,
        ) -> Self {
            Self::local("session", "session_started")
                .with_property("provider_id", serde_json::json!(provider_id))
                .with_property("model_id", serde_json::json!(model_id))
                .with_optional_property("execution_environment", execution_environment)
                .with_optional_property("session_root_kind", session_root_kind)
        }

        pub fn session_completed(
            provider_id: String,
            model_id: String,
            execution_environment: Option<String>,
            session_root_kind: Option<String>,
            status: String,
            duration_ms: u64,
        ) -> Self {
            Self::local("session", "session_completed")
                .with_property("provider_id", serde_json::json!(provider_id))
                .with_property("model_id", serde_json::json!(model_id))
                .with_optional_property("execution_environment", execution_environment)
                .with_optional_property("session_root_kind", session_root_kind)
                .with_property("status", serde_json::json!(status))
                .with_property("duration_ms", serde_json::json!(duration_ms))
        }

        pub fn provider_call(
            provider_id: String,
            model_id: String,
            execution_environment: Option<String>,
            session_root_kind: Option<String>,
            ok: bool,
            duration_ms: u64,
        ) -> Self {
            Self::local("provider", "provider_call")
                .with_property("provider_id", serde_json::json!(provider_id))
                .with_property("model_id", serde_json::json!(model_id))
                .with_optional_property("execution_environment", execution_environment)
                .with_optional_property("session_root_kind", session_root_kind)
                .with_property("ok", serde_json::json!(ok))
                .with_property("duration_ms", serde_json::json!(duration_ms))
        }

        pub fn session_interrupt_latency(
            provider_id: String,
            model_id: String,
            execution_environment: Option<String>,
            session_root_kind: Option<String>,
            duration_ms: u64,
            bucket: String,
        ) -> Self {
            Self::local("session", "session_interrupt_latency")
                .with_property("provider_id", serde_json::json!(provider_id))
                .with_property("model_id", serde_json::json!(model_id))
                .with_optional_property("execution_environment", execution_environment)
                .with_optional_property("session_root_kind", session_root_kind)
                .with_property("duration_ms", serde_json::json!(duration_ms))
                .with_property("bucket", serde_json::json!(bucket))
        }

        pub fn with_source(mut self, source: impl Into<String>) -> Self {
            self.source = Some(source.into());
            self
        }

        pub fn with_property(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
            self.properties.insert(key.into(), value);
            self
        }

        fn with_optional_property(self, key: impl Into<String>, value: Option<String>) -> Self {
            if let Some(value) = value {
                self.with_property(key, serde_json::json!(value))
            } else {
                self
            }
        }

        fn local(source: impl Into<String>, event_name: impl Into<String>) -> Self {
            Self {
                event_id: uuid_like_id(),
                event_name: event_name.into(),
                event_version: 1,
                occurred_at: Utc::now(),
                plane: TelemetryPlane::Operational,
                delivery: TelemetryDelivery::LocalOnly,
                origin_runtime: TelemetryOriginRuntime::Daemon,
                origin_install_id: None,
                app_version: env!("CARGO_PKG_VERSION").to_string(),
                os: std::env::consts::OS.to_string(),
                arch: std::env::consts::ARCH.to_string(),
                surface: None,
                env_target: None,
                source: Some(source.into()),
                properties: TelemetryProperties::new(),
            }
        }
    }

    fn uuid_like_id() -> String {
        format!(
            "evt-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        )
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct TelemetryConfig {
        pub enabled: bool,
        pub endpoint: String,
    }

    impl Default for TelemetryConfig {
        fn default() -> Self {
            Self {
                enabled: true,
                endpoint: String::new(),
            }
        }
    }

    #[derive(Debug, Clone)]
    pub struct Telemetry {
        data_root: PathBuf,
        config: Arc<Mutex<TelemetryConfig>>,
    }

    impl Telemetry {
        pub fn new(data_root: PathBuf) -> Self {
            Self {
                data_root,
                config: Arc::new(Mutex::new(TelemetryConfig::default())),
            }
        }

        pub async fn emit(&self, event: TelemetryEvent) {
            tracing::debug!(
                event_name = %event.event_name,
                data_root = %self.data_root.display(),
                "telemetry event"
            );
        }

        pub async fn emit_many(&self, events: Vec<TelemetryEvent>) {
            for event in events {
                self.emit(event).await;
            }
        }

        pub async fn update_config(&self, config: TelemetryConfig) {
            *self.config.lock().expect("telemetry mutex poisoned") = config;
        }
    }
}

pub mod provider_unknown_events {
    use super::*;
    use ctx_providers::adapters::ProviderUnknownEventHook;
    use ctx_providers::events::ProviderUnknownEventObservation;

    #[derive(Debug, Clone)]
    pub struct ProviderUnknownEvents {
        data_root: PathBuf,
    }

    impl ProviderUnknownEvents {
        pub fn new(data_root: PathBuf, _telemetry: crate::telemetry::Telemetry) -> Self {
            Self { data_root }
        }

        pub fn emit(
            &self,
            context: ProviderUnknownEventContext,
            observation: ProviderUnknownEventObservation,
        ) {
            tracing::debug!(
                provider_id = %context.provider_id,
                operation = %context.operation,
                event_type = %observation.event_type,
                data_root = %self.data_root.display(),
                "provider unknown event"
            );
        }
    }

    #[derive(Debug, Clone)]
    pub struct ProviderUnknownEventContext {
        pub provider_id: String,
        pub execution_environment: Option<String>,
        pub session_root_kind: Option<String>,
        pub operation: String,
    }

    pub fn provider_unknown_event_hook(
        events: ProviderUnknownEvents,
        context: ProviderUnknownEventContext,
    ) -> ProviderUnknownEventHook {
        Arc::new(move |observation| {
            let events = events.clone();
            let context = context.clone();
            Box::pin(async move {
                events.emit(context, observation);
            })
        })
    }
}
