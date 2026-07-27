use std::{path::PathBuf, sync::Mutex};

use chrono::{TimeZone, Utc};
use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;
use serde_json::json;
use tempfile::TempDir;

use super::*;
use crate::{provider::codex::catalog::catalog_codex_session_files, CodexSessionCatalogOptions};
use crate::{CodexSessionImportOptions, CODEX_SESSION_SOURCE_FORMAT};

#[test]
fn bounded_producers_preserve_source_page_order_and_release_all_memory() {
    let fixture = ProducerFixture::new(12, 95);
    let tasks = fixture.tasks();
    let config =
        CodexProducerConfig::new(CODEX_PRODUCER_MAX_WORKERS, CODEX_PREPARATION_HARD_BYTES).unwrap();
    let seen = Mutex::new(Vec::new());
    let stats = run_codex_bounded_producers(tasks, config, |item| {
        match item {
            CodexOrderedProducerItem::Step {
                source_ordinal,
                page_ordinal,
                ..
            }
            | CodexOrderedProducerItem::Failed {
                source_ordinal,
                page_ordinal,
                ..
            } => seen.lock().unwrap().push((source_ordinal, page_ordinal)),
        }
        Ok(())
    })
    .unwrap();

    let seen = seen.into_inner().unwrap();
    assert!(!seen.is_empty());
    assert!(seen.windows(2).all(|pair| {
        pair[0].0 < pair[1].0
            || (pair[0].0 == pair[1].0 && pair[0].1.saturating_add(1) == pair[1].1)
    }));
    assert!(stats.worker_count <= CODEX_PRODUCER_MAX_WORKERS);
    assert!(stats.max_concurrent_producers <= stats.worker_count);
    assert!(stats.peak_preparation_bytes <= CODEX_PREPARATION_HARD_BYTES);
    assert!(
        stats.peak_queued_windows
            <= stats
                .worker_count
                .saturating_mul(CODEX_SOURCE_MAX_QUEUED_WINDOWS)
    );
    assert_eq!(stats.final_preparation_bytes, 0);
    assert_eq!(stats.final_queued_windows, 0);
}

#[test]
fn saturated_max_worker_budget_guarantees_first_window_headroom() {
    let fixture = ProducerFixture::new(CODEX_PRODUCER_MAX_WORKERS, 95);
    let config =
        CodexProducerConfig::new(CODEX_PRODUCER_MAX_WORKERS, CODEX_PREPARATION_HARD_BYTES).unwrap();
    let mut completed_sources = 0_usize;

    let stats = run_codex_bounded_producers(fixture.tasks(), config, |item| {
        if matches!(
            item,
            CodexOrderedProducerItem::Step {
                step: CodexNativeProducerStep::Noop(_)
                    | CodexNativeProducerStep::Window {
                        source_done: true,
                        ..
                    },
                ..
            } | CodexOrderedProducerItem::Failed { .. }
        ) {
            completed_sources = completed_sources.saturating_add(1);
        }
        Ok(())
    })
    .unwrap();

    assert_eq!(
        stats.worker_count,
        CODEX_PREPARATION_HARD_BYTES / CODEX_PREPARE_RESERVATION_BYTES
    );
    assert_eq!(completed_sources, CODEX_PRODUCER_MAX_WORKERS);
    assert!(stats.peak_preparation_bytes <= CODEX_PREPARATION_HARD_BYTES);
    assert!(
        stats.peak_queued_windows
            <= stats
                .worker_count
                .saturating_mul(CODEX_SOURCE_MAX_QUEUED_WINDOWS)
    );
    assert_eq!(stats.final_preparation_bytes, 0);
    assert_eq!(stats.final_queued_windows, 0);
}

#[test]
fn worker_panic_is_immediate_and_joins_the_bounded_pool() {
    let fixture = ProducerFixture::new(4, 2);
    let config = CodexProducerConfig::new(4, CODEX_PREPARATION_HARD_BYTES)
        .unwrap()
        .with_panic_source(1);
    let error = run_codex_bounded_producers(fixture.tasks(), config, |_| Ok(())).unwrap_err();
    assert!(matches!(
        error,
        CaptureError::WorkerPanicked("Codex NativePath source preparation")
    ));
}

#[test]
fn consumer_cancellation_releases_waiting_producers() {
    let fixture = ProducerFixture::new(8, 95);
    let config = CodexProducerConfig::new(8, CODEX_PREPARATION_HARD_BYTES).unwrap();
    let error = run_codex_bounded_producers(fixture.tasks(), config, |_| {
        Err(CaptureError::SystemInvariant(
            "injected Codex consumer cancellation",
        ))
    })
    .unwrap_err();
    assert!(matches!(
        error,
        CaptureError::SystemInvariant("injected Codex consumer cancellation")
    ));
}

struct ProducerFixture {
    _temp: TempDir,
    store: Store,
    source_root: PathBuf,
    paths: Vec<PathBuf>,
}

impl ProducerFixture {
    fn new(source_count: usize, message_count: usize) -> Self {
        let temp = TempDir::new().unwrap();
        let source_root = temp.path().join("sessions");
        std::fs::create_dir_all(&source_root).unwrap();
        let paths = (0..source_count)
            .map(|index| {
                let path = source_root.join(format!("{index:03}.jsonl"));
                std::fs::write(
                    &path,
                    producer_fixture(&format!("producer-{index}"), message_count),
                )
                .unwrap();
                path
            })
            .collect::<Vec<_>>();
        let store = Store::open(temp.path().join("history.sqlite")).unwrap();
        catalog_codex_session_files(
            paths.clone(),
            &source_root,
            &store,
            CodexSessionCatalogOptions {
                source_root: Some(source_root.clone()),
                cataloged_at: Utc.with_ymd_and_hms(2026, 7, 26, 12, 0, 0).unwrap(),
                parallelism: Some(1),
                ..CodexSessionCatalogOptions::default()
            },
        )
        .unwrap();
        Self {
            _temp: temp,
            store,
            source_root,
            paths,
        }
    }

    fn tasks(&self) -> Vec<CodexNativeProducerTask> {
        let sessions = self
            .store
            .list_catalog_sessions_for_source(
                CaptureProvider::Codex,
                &self.source_root.display().to_string(),
            )
            .unwrap();
        let mut sources = super::super::discover_codex_catalog_sources(&sessions).sources;
        sources.sort_by(|left, right| left.source_path.cmp(&right.source_path));
        assert_eq!(sources.len(), self.paths.len());
        let options = super::super::CodexNativeStoreOptions {
            machine_id: "producer-test-machine".to_owned(),
            imported_at: Utc.with_ymd_and_hms(2026, 7, 26, 12, 0, 0).unwrap(),
            history_record_id: CodexSessionImportOptions::default().history_record_id,
        };
        sources
            .into_iter()
            .map(|source| {
                super::super::prepare_codex_native_producer_task(
                    &self.store,
                    source,
                    options.clone(),
                )
                .unwrap()
            })
            .collect()
    }
}

fn producer_fixture(session_id: &str, messages: usize) -> String {
    let mut lines = vec![json!({
        "timestamp": "2026-01-01T00:00:00Z",
        "type": "session_meta",
        "payload": {
            "id": session_id,
            "timestamp": "2026-01-01T00:00:00Z",
            "cwd": "/workspace",
            "source": "cli"
        }
    })];
    lines.extend((0..messages).map(|index| {
        json!({
            "timestamp": "2026-01-01T00:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": format!("{CODEX_SESSION_SOURCE_FORMAT}-{session_id}-{index}")
                }]
            }
        })
    }));
    lines
        .into_iter()
        .map(|line| format!("{}\n", serde_json::to_string(&line).unwrap()))
        .collect()
}
