use std::time::{Duration, Instant};

// Run with: cargo test -p ctx-http --test memory_leak_e2e -- --ignored --nocapture

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use serde_json::json;

use ctx_settings_model::{
    ProviderGuardSettings, ProviderRestartSettings, ResourceGovernanceMode, Settings,
};

mod common;

struct EnvGuard {
    key: &'static str,
    prev: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let prev = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, prev }
    }

    fn set_if_missing(key: &'static str, value: &str) -> Option<Self> {
        if std::env::var(key).is_ok() {
            return None;
        }
        Some(Self::set(key, value))
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(prev) = self.prev.take() {
            std::env::set_var(self.key, prev);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

#[derive(Debug)]
#[allow(dead_code)]
struct LeakReport {
    label: &'static str,
    start_rss: u64,
    end_rss: u64,
    peak_rss: u64,
    elapsed: Duration,
    slope_mb_per_min: f64,
}

fn env_u64(key: &str) -> Option<u64> {
    std::env::var(key)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
}

#[cfg(target_os = "linux")]
fn read_rss_bytes() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb = rest
                .split_whitespace()
                .next()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0);
            return kb.saturating_mul(1024);
        }
    }
    0
}

#[cfg(not(target_os = "linux"))]
fn read_rss_bytes() -> u64 {
    0
}

fn bytes_to_mb(value: u64) -> f64 {
    value as f64 / (1024.0 * 1024.0)
}

async fn run_scenario(label: &'static str, monitoring_enabled: bool) -> LeakReport {
    let _resource_guard = EnvGuard::set(
        "CTX_RESOURCE_UTILIZATION_DISABLED",
        if monitoring_enabled { "0" } else { "1" },
    );
    let _telemetry_interval =
        EnvGuard::set_if_missing("CTX_RESOURCE_TELEMETRY_INTERVAL_MS", "1000");
    let _telemetry_max = EnvGuard::set_if_missing("CTX_RESOURCE_TELEMETRY_LOCAL_MAX_BYTES", "0");
    let _telemetry_children = EnvGuard::set_if_missing("CTX_RESOURCE_TELEMETRY_CHILD_LIMIT", "0");

    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let daemon = &fixture.daemon;
    let app = fixture.router();

    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    let session_count = env_u64("CTX_MEMLEAK_SESSION_COUNT").unwrap_or(6);
    let mut sessions = Vec::new();
    for idx in 0..session_count {
        let (_task, session) = common::create_task_with_session(
            &app,
            ws.id.0,
            &format!("task-{idx}"),
            "fake",
            "fake-model",
        )
        .await;
        sessions.push(session);
    }

    let settings = Settings {
        provider_guard: Some(ProviderGuardSettings {
            enabled: monitoring_enabled,
            mode: ResourceGovernanceMode::Auto,
            memory_high_mb: None,
            memory_max_mb: None,
            interval_ms: Some(500),
            grace_period_ms: Some(60_000),
        }),
        provider_restart: Some(ProviderRestartSettings {
            enabled: monitoring_enabled,
            mode: ResourceGovernanceMode::Auto,
            memory_high_mb: None,
            memory_max_mb: None,
            interval_ms: Some(500),
            grace_period_ms: Some(60_000),
        }),
        ..Default::default()
    };
    daemon
        .apply_provider_monitoring_settings_for_test(&settings)
        .await
        .unwrap();
    daemon.spawn_provider_monitoring_for_test();

    let duration_secs = env_u64("CTX_MEMLEAK_DURATION_SECS").unwrap_or(60);
    let delay_ms = env_u64("CTX_MEMLEAK_MESSAGE_INTERVAL_MS").unwrap_or(50);
    let sample_every = env_u64("CTX_MEMLEAK_SAMPLE_EVERY").unwrap_or(10).max(1);
    let snapshot_every = env_u64("CTX_MEMLEAK_SNAPSHOT_EVERY").unwrap_or(25).max(1);

    let start_rss = read_rss_bytes();
    let start_time = Instant::now();
    let mut peak_rss = start_rss;

    let mut idx: u64 = 0;
    let deadline = start_time + Duration::from_secs(duration_secs);
    while Instant::now() < deadline {
        let session = &sessions[(idx as usize) % sessions.len()];
        let (status, _msg): (StatusCode, ctx_core::models::Message) = common::json_request(
            &app,
            Method::POST,
            format!("/api/sessions/{}/messages", session.id.0),
            Some(json!({"content": format!("ping {idx} ({label})")})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        if idx.is_multiple_of(snapshot_every) {
            let req = Request::builder()
                .method(Method::GET)
                .uri(format!(
                    "/api/workspaces/{}/active_snapshot?limit=25",
                    ws.id.0
                ))
                .body(Body::empty())
                .unwrap();
            let (status, _snapshot): (StatusCode, serde_json::Value) =
                common::oneshot_json(&app, req).await;
            assert_eq!(status, StatusCode::OK);
        }

        if idx.is_multiple_of(sample_every) {
            peak_rss = peak_rss.max(read_rss_bytes());
        }

        idx += 1;
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
    }

    let end_rss = read_rss_bytes();
    let elapsed = start_time.elapsed();
    let elapsed_min = elapsed.as_secs_f64() / 60.0;
    let slope_mb_per_min = if elapsed_min > 0.0 {
        (bytes_to_mb(end_rss.saturating_sub(start_rss))) / elapsed_min
    } else {
        0.0
    };

    println!(
        "memleak {label}: start={:.1}MB end={:.1}MB peak={:.1}MB elapsed={:.1}s slope={:.1}MB/min",
        bytes_to_mb(start_rss),
        bytes_to_mb(end_rss),
        bytes_to_mb(peak_rss),
        elapsed.as_secs_f64(),
        slope_mb_per_min
    );

    daemon.request_shutdown();

    LeakReport {
        label,
        start_rss,
        end_rss,
        peak_rss,
        elapsed,
        slope_mb_per_min,
    }
}

#[tokio::test]
#[ignore]
async fn memory_leak_baseline_monitoring_on() {
    if !cfg!(target_os = "linux") {
        eprintln!("memory leak tests only run on linux");
        return;
    }
    let report = run_scenario("monitoring_on", true).await;
    let max_mb_per_min = env_u64("CTX_MEMLEAK_MAX_MB_PER_MIN").unwrap_or(25) as f64;
    assert!(
        report.slope_mb_per_min <= max_mb_per_min,
        "memory growth {:.1}MB/min exceeded limit {:.1}MB/min",
        report.slope_mb_per_min,
        max_mb_per_min
    );
}

#[tokio::test]
#[ignore]
async fn memory_leak_monitoring_off() {
    if !cfg!(target_os = "linux") {
        eprintln!("memory leak tests only run on linux");
        return;
    }
    let report = run_scenario("monitoring_off", false).await;
    let max_mb_per_min = env_u64("CTX_MEMLEAK_MAX_MB_PER_MIN").unwrap_or(25) as f64;
    assert!(
        report.slope_mb_per_min <= max_mb_per_min,
        "memory growth {:.1}MB/min exceeded limit {:.1}MB/min",
        report.slope_mb_per_min,
        max_mb_per_min
    );
}
