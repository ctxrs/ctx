use crate::{
    catalog_codex_session_tree, import_codex_session_paths, CatalogSummary,
    CodexSessionCatalogOptions, CodexSessionImportOptions, ProviderImportSummary,
};
use chrono::{DateTime, Utc};
use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

pub(super) fn synthetic_codex_session_tree(root: &Path, sessions: usize) -> u64 {
    (0..sessions)
        .map(|index| write_synthetic_codex_session(root, index, "baseline"))
        .sum()
}

pub(super) fn write_synthetic_codex_session(root: &Path, index: usize, marker: &str) -> u64 {
    let shard = format!("{:02}", index / 1000);
    let dir = root.join("2026").join("06").join("26").join(shard);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("synthetic-session-{index:06}.jsonl"));
    let seconds = index % 86_400;
    let timestamp = format!(
        "2026-06-26T{:02}:{:02}:{:02}.000Z",
        seconds / 3600,
        (seconds / 60) % 60,
        seconds % 60
    );
    let session_id = format!("synthetic-codex-session-{index:06}");
    let meta = json!({
        "timestamp": timestamp,
        "type": "session_meta",
        "payload": {
            "id": session_id,
            "timestamp": timestamp,
            "cwd": "/repo/ctx",
            "originator": "codex-cli",
            "cli_version": "0.2.0-test",
            "source": "cli",
            "model_provider": "openai"
        }
    });
    let message = json!({
        "timestamp": timestamp,
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": format!("incremental import synthetic corpus {index:06} {marker}")
            }]
        }
    });
    let body = format!("{meta}\n{message}\n");
    fs::write(&path, body.as_bytes()).unwrap();
    body.len() as u64
}

#[derive(Debug)]
pub(super) struct IncrementalCatchUpSummary {
    pub(super) catalog: CatalogSummary,
    pub(super) import: ProviderImportSummary,
    pub(super) pending_sessions: usize,
}

pub(super) fn incremental_codex_catch_up(
    root: &Path,
    store: &mut Store,
    observed_at: DateTime<Utc>,
) -> IncrementalCatchUpSummary {
    let source_root = root.display().to_string();
    let catalog = catalog_codex_session_tree(
        root,
        store,
        CodexSessionCatalogOptions {
            source_root: Some(root.to_path_buf()),
            cataloged_at: observed_at,
            ..CodexSessionCatalogOptions::default()
        },
    )
    .unwrap();
    let pending = store
        .list_pending_catalog_sessions(CaptureProvider::Codex, &source_root)
        .unwrap();
    let pending_sessions = pending.len();
    if pending.is_empty() {
        return IncrementalCatchUpSummary {
            catalog,
            import: ProviderImportSummary::default(),
            pending_sessions,
        };
    }

    let paths = pending
        .iter()
        .map(|session| PathBuf::from(&session.source_path))
        .collect::<Vec<_>>();
    let import = import_codex_session_paths(
        paths,
        store,
        CodexSessionImportOptions {
            source_path: Some(root.to_path_buf()),
            imported_at: observed_at,
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();
    let indexed_at_ms = observed_at.timestamp_millis();
    for session in pending {
        store
            .mark_catalog_source_observation_indexed(&session, None, Some(1), indexed_at_ms)
            .unwrap();
    }

    IncrementalCatchUpSummary {
        catalog,
        import,
        pending_sessions,
    }
}

#[derive(Debug)]
pub(super) struct TimingStats {
    pub(super) min_ms: f64,
    pub(super) p50_ms: f64,
    pub(super) p95_ms: f64,
    pub(super) max_ms: f64,
}

impl TimingStats {
    pub(super) fn to_json(&self) -> Value {
        json!({
            "min_ms": rounded(self.min_ms),
            "p50_ms": rounded(self.p50_ms),
            "p95_ms": rounded(self.p95_ms),
            "max_ms": rounded(self.max_ms),
        })
    }
}

pub(super) fn timing_stats(samples: &[f64]) -> TimingStats {
    assert!(!samples.is_empty(), "timing samples must not be empty");
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    TimingStats {
        min_ms: sorted[0],
        p50_ms: percentile(&sorted, 0.50),
        p95_ms: percentile(&sorted, 0.95),
        max_ms: *sorted.last().unwrap(),
    }
}

pub(super) fn percentile(sorted: &[f64], percentile: f64) -> f64 {
    let index = ((sorted.len() - 1) as f64 * percentile).ceil() as usize;
    sorted[index.min(sorted.len() - 1)]
}

pub(super) fn elapsed_ms(duration: std::time::Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

pub(super) fn rounded(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

pub(super) fn env_flag(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| {
        let value = value.to_string_lossy();
        !matches!(value.as_ref(), "" | "0" | "false" | "False" | "FALSE")
    })
}

pub(super) fn env_usize(name: &str) -> Option<usize> {
    std::env::var(name).ok()?.parse().ok()
}

pub(super) fn env_f64(name: &str) -> Option<f64> {
    std::env::var(name).ok()?.parse().ok()
}

pub(super) fn incremental_perf_file_count() -> usize {
    env_usize("CTX_CODEX_INCREMENTAL_PERF_FILES").unwrap_or_else(|| {
        if env_flag("CTX_CODEX_INCREMENTAL_PERF_SLOW") {
            32_000
        } else {
            5_000
        }
    })
}

pub(super) fn incremental_perf_repeats() -> usize {
    env_usize("CTX_CODEX_INCREMENTAL_PERF_REPEATS")
        .unwrap_or(5)
        .max(1)
}

pub(super) fn incremental_perf_noop_p95_threshold_ms(file_count: usize) -> f64 {
    env_f64("CTX_CODEX_INCREMENTAL_PERF_NOOP_P95_MS").unwrap_or({
        if file_count >= 30_000 {
            1_000.0
        } else {
            500.0
        }
    })
}

pub(super) fn incremental_perf_noop_us_per_file_threshold() -> f64 {
    env_f64("CTX_CODEX_INCREMENTAL_PERF_NOOP_US_PER_FILE").unwrap_or(50.0)
}
