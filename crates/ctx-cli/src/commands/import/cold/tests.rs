use std::{fs, time::Instant};

use ctx_history_core::CaptureProvider;
use serde_json::json;
use tempfile::tempdir;

use super::*;
use crate::{
    analytics::{CountBucket, ProviderRefreshSourceMode, ProviderRefreshTrigger, PublicEventV1},
    commands::import::ProviderRefreshCollector,
    progress::ProgressArg,
    provider_sources::explicit_path_source,
    ImportArgs,
};

fn line(value: serde_json::Value) -> String {
    format!("{value}\n")
}

fn codex_session(session_id: &str) -> String {
    [
        line(json!({
            "timestamp": "2026-07-27T00:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": session_id,
                "timestamp": "2026-07-27T00:00:00Z",
                "cwd": "/workspace",
                "source": "cli"
            }
        })),
        line(json!({
            "timestamp": "2026-07-27T00:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "cold mixed provider"}]
            }
        })),
    ]
    .concat()
}

fn all_args() -> ImportArgs {
    ImportArgs {
        provider: None,
        path: None,
        history_source: None,
        history_source_manifest: Vec::new(),
        reset_cursor: false,
        input_format: None,
        all: true,
        resume: false,
        partial: false,
        no_daemon: true,
        format: crate::output::JsonOutputFormat::Text,
        progress: ProgressArg::None,
    }
}

fn options() -> ImportRunOptions {
    ImportRunOptions {
        progress: ProgressArg::None,
        json: false,
        print_human: false,
        allow_empty_sources: false,
        include_history_source_plugins: false,
        operation: "cold-import-test",
    }
}

#[test]
fn all_inventory_cold_seed_consumes_only_codex_and_preserves_combined_authority() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("session.jsonl"),
        codex_session("019f5a54-67de-7422-9841-e9872df75f44"),
    )
    .unwrap();
    let history = temp.path().join("history.jsonl");
    fs::write(
        &history,
        line(json!({
            "session_id": "019f5a54-67de-7422-9841-e9872df75f45",
            "ts": 1785110400,
            "text": "prompt history session"
        })),
    )
    .unwrap();
    let other_path = temp.path().join("other.db");
    fs::write(&other_path, b"ordinary-provider-placeholder").unwrap();

    let mut requests = vec![
        explicit_path_source(CaptureProvider::Codex, sessions),
        explicit_path_source(CaptureProvider::Hermes, other_path.clone()),
        explicit_path_source(CaptureProvider::Codex, history),
    ];
    let db_path = temp.path().join("ctx.db");
    let mut refreshes = ProviderRefreshCollector::default();
    let outer_started = Instant::now();
    let seed = try_codex_cold_cli_import(
        &all_args(),
        &requests,
        &db_path,
        &mut refreshes,
        ProviderRefreshTrigger::Setup,
        &options(),
    )
    .unwrap()
    .expect("fresh all-provider inventory should cold-seed Codex");
    let outer_elapsed = outer_started.elapsed();

    assert_eq!(seed.consumed_sources.len(), 2);
    assert_eq!(seed.report.inventory.sources, 2);
    assert_eq!(seed.report.inventory.source_files, 2);
    assert_eq!(seed.report.totals.imported_sources, 2);
    assert_eq!(seed.report.totals.imported_sessions, 2);
    seed.remove_consumed_from(&mut requests);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].provider, CaptureProvider::Hermes);
    assert_eq!(requests[0].path, other_path);

    let store = ctx_history_store::Store::open_read_only(&db_path).unwrap();
    assert_eq!(store.list_sessions().unwrap().len(), 2);
    assert_eq!(store.list_capture_sources().unwrap().len(), 2);

    let recorded_duration = refreshes
        .recorded_duration(
            CaptureProvider::Codex,
            ProviderRefreshTrigger::Setup,
            ProviderRefreshSourceMode::Discovered,
        )
        .expect("cold collector must retain its one combined duration");
    assert!(recorded_duration > std::time::Duration::ZERO);
    assert!(
        recorded_duration <= outer_elapsed,
        "two source summaries must not add the same cold duration twice"
    );
    let events = refreshes.finish();
    assert_eq!(events.len(), 1);
    let PublicEventV1::ProviderRefreshCompleted(event) = &events[0] else {
        panic!("cold collector emitted the wrong event family");
    };
    let refresh = event.foreground.as_ref().unwrap();
    assert_eq!(refresh.counts.sources, CountBucket::TwoToFive);
    assert!(
        refresh.performance.is_some(),
        "the real cold path must attach its trusted process resource receipt"
    );
}

#[test]
fn existing_target_never_uses_the_cold_seed() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("session.jsonl"),
        codex_session("019f5a54-67de-7422-9841-e9872df75f46"),
    )
    .unwrap();
    let requests = vec![explicit_path_source(CaptureProvider::Codex, sessions)];
    let db_path = temp.path().join("ctx.db");
    ctx_history_store::Store::open(&db_path).unwrap();

    let mut refreshes = ProviderRefreshCollector::default();
    let seed = try_codex_cold_cli_import(
        &all_args(),
        &requests,
        &db_path,
        &mut refreshes,
        ProviderRefreshTrigger::Setup,
        &options(),
    )
    .unwrap();

    assert!(seed.is_none());
    assert_eq!(requests.len(), 1);
}

#[test]
fn deprecated_partial_flag_does_not_disable_cold_admission() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("session.jsonl"),
        codex_session("019f5a54-67de-7422-9841-e9872df75f47"),
    )
    .unwrap();
    let requests = vec![explicit_path_source(CaptureProvider::Codex, sessions)];
    let mut args = all_args();
    args.partial = true;
    let mut refreshes = ProviderRefreshCollector::default();

    let seed = try_codex_cold_cli_import(
        &args,
        &requests,
        &temp.path().join("ctx.db"),
        &mut refreshes,
        ProviderRefreshTrigger::Import,
        &options(),
    )
    .unwrap();

    assert!(seed.is_some());
}
