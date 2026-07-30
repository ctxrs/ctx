use std::path::Path;

use serde_json::{json, Value};

use crate::{
    config::{AppConfig, DaemonMode},
    output::JsonOutputFormat,
    semantic::{
        daemon::{install_daemon_test_job_hooks, DaemonTestJobHooks},
        source_backed_refresh_coordinator::source_backed_index_root,
    },
    DaemonRunArgs,
};

use super::{
    daemon_job_should_backoff, daemon_mode_runs_source_backed_pro_catch_up,
    daemon_mode_runs_source_backed_relational_catch_up,
    daemon_mode_runs_source_backed_semantic_projection, daemon_semantic_job_path,
    daemon_source_backed_refresh_job_path, persist_pro_status, persist_relational_status,
    prepare_pro_retry_for_generation, read_daemon_job_status, read_pro_status,
    record_daemon_job_retry, restore_daemon_consumer_retries, run_daemon_once_with_activity,
    run_pro_catch_up_with_retry, write_daemon_job_status, DaemonRetryBackoff, DaemonRuntime,
};

fn daemon_args() -> DaemonRunArgs {
    DaemonRunArgs {
        foreground: false,
        once: true,
        idle_exit_seconds: None,
        loop_interval_seconds: None,
        max_chunks: None,
        max_seconds: None,
        force: false,
        start_mode: None,
        trigger_command: None,
        format: JsonOutputFormat::Json,
    }
}

fn publish_empty_core_generation(data_root: &Path) -> String {
    ctx_history_index::GenerationWriter::open(
        source_backed_index_root(data_root),
        ctx_history_index::WriterOptions::default(),
    )
    .unwrap()
    .commit(|_| true)
    .unwrap()
    .generation_id
}

fn relational_status(generation: &str, status: &str) -> Value {
    let completed = status == "completed";
    json!({
        "schema_version": 1,
        "owner": "daemon",
        "kind": "source_backed_relational_catch_up",
        "status": status,
        "pending": !completed,
        "retryable": !completed,
        "core_generation_id": generation,
        "active_core_generation_id": completed.then_some(generation),
        "receipt_core_generation_id": completed.then_some(generation),
        "projection_status": completed.then_some("ready"),
        "build_generation": completed.then_some(1),
        "attempts": 1,
        "last_attempt_at_ms": 1,
        "error_code": (!completed).then_some("injected_failure"),
        "last_error": (!completed).then_some("injected relational failure"),
    })
}

fn install_jobs(
    calls: std::rc::Rc<std::cell::RefCell<Vec<&'static str>>>,
    relational_projection: Option<Value>,
    semantic_index: Option<Value>,
) -> super::super::daemon::DaemonTestJobHookGuard {
    install_daemon_test_job_hooks(DaemonTestJobHooks {
        calls,
        history_refresh: None,
        relational_projection,
        semantic_index,
    })
}

#[test]
fn source_refresh_only_mode_excludes_source_backed_pro_catch_up() {
    assert!(daemon_mode_runs_source_backed_pro_catch_up(
        DaemonMode::Full
    ));
    assert!(!daemon_mode_runs_source_backed_pro_catch_up(
        DaemonMode::SourceRefreshOnly
    ));
}

#[test]
fn source_refresh_only_mode_excludes_source_backed_relational_catch_up() {
    assert!(daemon_mode_runs_source_backed_relational_catch_up(
        DaemonMode::Full
    ));
    assert!(!daemon_mode_runs_source_backed_relational_catch_up(
        DaemonMode::SourceRefreshOnly
    ));
}

#[test]
fn source_refresh_only_mode_excludes_source_backed_semantic_projection() {
    assert!(daemon_mode_runs_source_backed_semantic_projection(
        DaemonMode::Full
    ));
    assert!(!daemon_mode_runs_source_backed_semantic_projection(
        DaemonMode::SourceRefreshOnly
    ));
}

#[test]
fn source_refresh_only_tick_creates_no_consumer_catch_up_status() {
    let temp = tempfile::tempdir().unwrap();
    let mut config = AppConfig::default();
    config.daemon.mode = DaemonMode::SourceRefreshOnly;
    let mut runtime = DaemonRuntime {
        config,
        ..DaemonRuntime::default()
    };
    let args = DaemonRunArgs {
        foreground: false,
        once: true,
        idle_exit_seconds: None,
        loop_interval_seconds: None,
        max_chunks: None,
        max_seconds: None,
        force: false,
        start_mode: None,
        trigger_command: None,
        format: JsonOutputFormat::Json,
    };

    let iteration =
        run_daemon_once_with_activity(&args, temp.path(), &mut runtime, None, true, None, None)
            .unwrap();

    assert!(!iteration.did_work);
    assert!(!iteration.failed);
    assert!(!temp.path().join("daemon/jobs/pro-catch-up.json").exists());
    assert!(!temp
        .path()
        .join("daemon/jobs/relational-catch-up.json")
        .exists());
    assert!(!super::daemon_semantic_job_path(temp.path()).exists());
}

#[test]
fn pro_projection_error_never_puts_core_refresh_into_backoff() {
    let core_job = json!({
        "status": "completed",
        "published_generation": "a".repeat(64),
        "pro_projection": {
            "status": "error",
            "pending": true,
            "retryable": true,
            "error_code": "pro_not_installed",
        },
    });
    let mut backoff = DaemonRetryBackoff::default();

    assert!(!daemon_job_should_backoff(&core_job));
    let recorded = record_daemon_job_retry(&mut backoff, core_job);

    assert_eq!(recorded["status"], "completed");
    assert_eq!(recorded["pro_projection"]["status"], "error");
    assert_eq!(backoff.consecutive_failures, 0);
}

fn failed_pro_status(generation: &str) -> Value {
    json!({
        "schema_version": 1,
        "owner": "daemon",
        "kind": "source_backed_pro_catch_up",
        "status": "error",
        "pending": true,
        "retryable": true,
        "core_generation_id": generation,
        "receipt_core_generation_id": null,
        "attempts": 1,
        "last_attempt_at_ms": 1,
        "error_code": "helper_crashed",
        "last_error": "fixture failure",
    })
}

#[test]
fn pro_failure_backoff_is_independent_and_skips_until_due() {
    let temp = tempfile::tempdir().unwrap();
    let generation = "a".repeat(64);
    let mut runtime = DaemonRuntime::default();
    runtime.history_retry.record_failure();
    runtime.semantic_retry.record_failure();
    let history_failures = runtime.history_retry.consecutive_failures;
    let semantic_failures = runtime.semantic_retry.consecutive_failures;

    let status = record_daemon_job_retry(&mut runtime.pro_retry, failed_pro_status(&generation));
    persist_pro_status(temp.path(), &status).unwrap();
    assert_eq!(runtime.pro_retry.consecutive_failures, 1);
    assert!(!runtime.pro_retry.ready());
    assert_eq!(runtime.history_retry.consecutive_failures, history_failures);
    assert_eq!(
        runtime.semantic_retry.consecutive_failures,
        semantic_failures
    );

    let skipped =
        run_pro_catch_up_with_retry(temp.path(), &mut runtime, &generation, None).unwrap();
    assert!(!skipped.did_work);
    assert_eq!(skipped.status["reason"], "retry_backoff");
    assert_eq!(skipped.status["consecutive_failures"], 1);
    assert_eq!(read_pro_status(temp.path()).unwrap()["status"], "error");
}

#[test]
fn pro_retry_restores_across_restart_and_core_backoff_does_not_gate_it() {
    let temp = tempfile::tempdir().unwrap();
    let generation = "b".repeat(64);
    let mut first = DaemonRuntime::default();
    let status = record_daemon_job_retry(&mut first.pro_retry, failed_pro_status(&generation));
    persist_pro_status(temp.path(), &status).unwrap();

    let mut restarted = DaemonRuntime::default();
    restarted.history_retry.record_failure();
    restore_daemon_consumer_retries(&mut restarted, temp.path());
    assert!(!restarted.history_retry.ready());
    assert!(!restarted.pro_retry.ready());
    assert_eq!(restarted.pro_retry.consecutive_failures, 1);

    restarted.pro_retry.retry_not_before = None;
    restarted.pro_retry.retry_not_before_at_ms = None;
    prepare_pro_retry_for_generation(&mut restarted, temp.path(), &generation);
    assert!(
        restarted.pro_retry.ready(),
        "Core history backoff must not block a due Pro retry"
    );
    assert!(!restarted.history_retry.ready());
}

#[test]
fn successful_pro_retry_resets_only_pro_state() {
    let mut runtime = DaemonRuntime::default();
    runtime.history_retry.record_failure();
    runtime.semantic_retry.record_failure();
    runtime.pro_retry.record_failure();
    let history_failures = runtime.history_retry.consecutive_failures;
    let semantic_failures = runtime.semantic_retry.consecutive_failures;

    let completed = record_daemon_job_retry(
        &mut runtime.pro_retry,
        json!({
            "status": "completed",
            "pending": false,
            "retryable": false,
        }),
    );
    assert_eq!(completed["status"], "completed");
    assert_eq!(runtime.pro_retry.consecutive_failures, 0);
    assert_eq!(runtime.history_retry.consecutive_failures, history_failures);
    assert_eq!(
        runtime.semantic_retry.consecutive_failures,
        semantic_failures
    );
}

#[test]
fn relational_projection_error_never_puts_core_refresh_into_backoff() {
    let core_job = json!({
        "status": "completed",
        "published_generation": "a".repeat(64),
        "relational_projection": {
            "status": "error",
            "pending": true,
            "retryable": true,
            "error_code": "source_relational_projection_unavailable",
        },
    });
    let mut backoff = DaemonRetryBackoff::default();

    assert!(!daemon_job_should_backoff(&core_job));
    let recorded = record_daemon_job_retry(&mut backoff, core_job);

    assert_eq!(recorded["status"], "completed");
    assert_eq!(recorded["relational_projection"]["status"], "error");
    assert_eq!(backoff.consecutive_failures, 0);
}

#[test]
fn relational_retry_runs_across_core_noop_backoff_and_recovers_independently() {
    let temp = tempfile::tempdir().unwrap();
    let generation = publish_empty_core_generation(temp.path());
    write_daemon_job_status(
        &daemon_source_backed_refresh_job_path(temp.path()),
        &json!({
            "status": "completed",
            "reason": "unchanged",
            "published_generation": generation,
        }),
    )
    .unwrap();
    let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut first = DaemonRuntime::default();
    first.history_retry.record_failure();
    let history_failures = first.history_retry.consecutive_failures;
    {
        let _hooks = install_jobs(
            calls.clone(),
            Some(relational_status(&generation, "error")),
            None,
        );
        let iteration = run_daemon_once_with_activity(
            &daemon_args(),
            temp.path(),
            &mut first,
            None,
            false,
            None,
            None,
        )
        .unwrap();
        assert!(!iteration.failed, "derived failure cannot revoke Core");
    }
    assert_eq!(&*calls.borrow(), &["relational_projection"]);
    assert_eq!(first.relational_retry.consecutive_failures, 1);
    assert_eq!(first.history_retry.consecutive_failures, history_failures);
    assert_eq!(first.semantic_retry.consecutive_failures, 0);

    let mut restarted = DaemonRuntime::default();
    restarted.history_retry.record_failure();
    restore_daemon_consumer_retries(&mut restarted, temp.path());
    assert!(!restarted.relational_retry.ready());
    restarted.relational_retry.retry_not_before = None;
    restarted.relational_retry.retry_not_before_at_ms = None;
    calls.borrow_mut().clear();
    {
        let _hooks = install_jobs(
            calls.clone(),
            Some(relational_status(&generation, "completed")),
            None,
        );
        let iteration = run_daemon_once_with_activity(
            &daemon_args(),
            temp.path(),
            &mut restarted,
            None,
            false,
            None,
            None,
        )
        .unwrap();
        assert!(!iteration.failed);
    }
    assert_eq!(&*calls.borrow(), &["relational_projection"]);
    assert_eq!(restarted.relational_retry.consecutive_failures, 0);
    assert_eq!(restarted.history_retry.consecutive_failures, 1);
    assert_eq!(
        read_daemon_job_status(&daemon_source_backed_refresh_job_path(temp.path())).unwrap()
            ["status"],
        "completed"
    );
}

#[test]
fn semantic_retry_runs_across_core_backoff_while_relational_waits_and_recovers_alone() {
    let temp = tempfile::tempdir().unwrap();
    let generation = publish_empty_core_generation(temp.path());
    let mut relational_retry = DaemonRetryBackoff::default();
    let relational = record_daemon_job_retry(
        &mut relational_retry,
        relational_status(&generation, "error"),
    );
    persist_relational_status(temp.path(), &relational).unwrap();
    write_daemon_job_status(
        &daemon_source_backed_refresh_job_path(temp.path()),
        &json!({
            "status": "completed",
            "reason": "unchanged",
            "published_generation": generation,
        }),
    )
    .unwrap();

    let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut first = DaemonRuntime::default();
    first.history_retry.record_failure();
    first.relational_retry.restore(Some(&relational));
    {
        let _hooks = install_jobs(
            calls.clone(),
            None,
            Some(json!({
                "status": "failed",
                "failure_class": "retryable",
                "retryable": true,
                "last_error": "injected semantic failure",
            })),
        );
        let iteration = run_daemon_once_with_activity(
            &daemon_args(),
            temp.path(),
            &mut first,
            None,
            true,
            None,
            None,
        )
        .unwrap();
        assert!(!iteration.failed, "semantic failure cannot revoke Core");
    }
    assert_eq!(&*calls.borrow(), &["semantic_index"]);
    assert_eq!(first.semantic_retry.consecutive_failures, 1);
    assert_eq!(first.relational_retry.consecutive_failures, 1);
    assert_eq!(first.history_retry.consecutive_failures, 1);

    let mut restarted = DaemonRuntime::default();
    restarted.history_retry.record_failure();
    restore_daemon_consumer_retries(&mut restarted, temp.path());
    assert!(!restarted.relational_retry.ready());
    assert!(!restarted.semantic_retry.ready());
    restarted.semantic_retry.retry_not_before = None;
    restarted.semantic_retry.retry_not_before_at_ms = None;
    calls.borrow_mut().clear();
    {
        let _hooks = install_jobs(
            calls.clone(),
            None,
            Some(json!({
                "status": "ready",
                "source_generation_ready": true,
                "source_work_remaining": false,
            })),
        );
        let iteration = run_daemon_once_with_activity(
            &daemon_args(),
            temp.path(),
            &mut restarted,
            None,
            true,
            None,
            None,
        )
        .unwrap();
        assert!(!iteration.failed);
    }
    assert_eq!(&*calls.borrow(), &["semantic_index"]);
    assert_eq!(restarted.semantic_retry.consecutive_failures, 0);
    assert_eq!(restarted.relational_retry.consecutive_failures, 1);
    assert_eq!(restarted.history_retry.consecutive_failures, 1);
    let semantic = read_daemon_job_status(&daemon_semantic_job_path(temp.path())).unwrap();
    assert_eq!(semantic["status"], "ready");
    assert_eq!(semantic["core_generation_id"], generation);
}

#[test]
fn semantic_projection_error_never_puts_core_refresh_into_backoff() {
    let core_job = json!({
        "status": "completed",
        "published_generation": "a".repeat(64),
        "semantic_projection": {
            "status": "failed",
            "retryable": true,
            "failure_class": "transient",
            "last_error": "fixture semantic failure",
        },
    });
    let mut backoff = DaemonRetryBackoff::default();

    assert!(!daemon_job_should_backoff(&core_job));
    let recorded = record_daemon_job_retry(&mut backoff, core_job);

    assert_eq!(recorded["status"], "completed");
    assert_eq!(recorded["semantic_projection"]["status"], "failed");
    assert_eq!(backoff.consecutive_failures, 0);
}
