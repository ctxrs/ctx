// Contract tests for the fixed Core capability protocol.
use super::progress_events::{CapabilityEventSink, IgnoreEvents, ProtocolEventWriter};
use super::*;
use ctx_history_index::SourceRouteIdentity;
use ctx_history_refresh::{RefreshOutcomeCode, RefreshRetryAdvice, RefreshTerminalOutcome};

#[cfg(test)]
#[path = "contract_tests/failure_contract_tests.rs"]
mod failure_contract_tests;

#[cfg(test)]
#[path = "contract_tests/managed_pair_apply_contract_tests.rs"]
mod managed_pair_apply_contract_tests;

#[test]
fn fingerprint_is_the_sha256_of_the_canonical_inventory() {
    assert_eq!(
        format!("{:x}", Sha256::digest(API_INVENTORY.as_bytes())),
        API_FINGERPRINT
    );
    std::println!("CTX_MANAGED_PAIR_CORE_CAPABILITY_FINGERPRINT={API_FINGERPRINT}");
}

#[test]
fn wake_refresh_reports_resolved_analytics_consent_fail_closed() {
    let _lock = ctx_app_config::TEST_LOCAL_USAGE_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let names = [
        "CTX_ANALYTICS_ENABLED",
        "CTX_ANALYTICS_ENDPOINT",
        "CTX_ANALYTICS_OFF",
        "CTX_DISABLE_ANALYTICS",
        "CTX_INSTALL_DIAGNOSTICS_OFF",
    ];
    let saved = names
        .iter()
        .map(|name| (*name, std::env::var_os(name)))
        .collect::<Vec<_>>();
    for name in names {
        std::env::remove_var(name);
    }
    struct Restore(Vec<(&'static str, Option<std::ffi::OsString>)>);
    impl Drop for Restore {
        fn drop(&mut self) {
            for (name, value) in self.0.drain(..) {
                if let Some(value) = value {
                    std::env::set_var(name, value);
                } else {
                    std::env::remove_var(name);
                }
            }
        }
    }
    let _restore = Restore(saved);

    let root = tempfile::tempdir().unwrap();
    let receipt = |config: &str| {
        std::fs::write(root.path().join(ctx_app_config::CONFIG_FILE), config).unwrap();
        execute(
            Request {
                data_root: root.path().to_path_buf(),
                operation: Operation::WakeRefresh,
                options: Options::Empty,
            },
            &mut IgnoreEvents,
        )
        .unwrap()
    };

    assert_eq!(
        receipt("[analytics]\nenabled = true\n[indexing]\nmode = \"manual\"\n")["facts"],
        json!({"accepted": true, "analytics_enabled": true})
    );
    assert_eq!(
        receipt("[analytics]\nenabled = false\n[indexing]\nmode = \"manual\"\n")["facts"],
        json!({"accepted": true, "analytics_enabled": false})
    );

    std::env::set_var("CTX_ANALYTICS_ENABLED", "false");
    assert_eq!(
        receipt("[analytics]\nenabled = true\n[indexing]\nmode = \"manual\"\n")["facts"],
        json!({"accepted": true, "analytics_enabled": false})
    );

    std::env::set_var("CTX_ANALYTICS_ENABLED", "true");
    assert_eq!(
        receipt("[analytics]\nenabled = true\n[indexing]\nmode = \"manual\"\n")["facts"],
        json!({"accepted": true, "analytics_enabled": true})
    );

    for value in ["", "malformed", "2"] {
        std::env::set_var("CTX_ANALYTICS_ENABLED", value);
        assert_eq!(
            receipt("[analytics]\nenabled = true\n[indexing]\nmode = \"manual\"\n")["facts"],
            json!({"accepted": true, "analytics_enabled": false}),
            "override {value:?} must fail closed"
        );
    }

    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt as _;
        std::env::set_var(
            "CTX_ANALYTICS_ENABLED",
            std::ffi::OsString::from_vec(vec![0xff]),
        );
        assert_eq!(
            receipt("[analytics]\nenabled = true\n[indexing]\nmode = \"manual\"\n")["facts"],
            json!({"accepted": true, "analytics_enabled": false})
        );
    }

    std::env::remove_var("CTX_ANALYTICS_ENABLED");
    for alias in [
        "CTX_ANALYTICS_OFF",
        "CTX_DISABLE_ANALYTICS",
        "CTX_INSTALL_DIAGNOSTICS_OFF",
    ] {
        std::env::set_var(alias, "yes");
        assert_eq!(
            receipt("[analytics]\nenabled = true\n[indexing]\nmode = \"manual\"\n")["facts"],
            json!({"accepted": true, "analytics_enabled": false}),
            "deprecated alias {alias} must fail closed"
        );
        std::env::remove_var(alias);
    }

    std::env::set_var("CTX_ANALYTICS_ENABLED", "true");
    assert_eq!(
        receipt("[analytics]\nenabled = malformed\n")["facts"],
        json!({"accepted": true, "analytics_enabled": false})
    );
}

#[test]
fn duplicates_and_multiframe_input_fail_closed() {
    assert!(reject_duplicate_keys(r#"{"a":1,"a":2}"#).is_err());
    assert!(parse_frame(b"{}\n{}".to_vec()).is_err());
}

#[test]
fn capability_response_is_one_exact_flushed_json_frame() {
    let mut output = Vec::new();
    write_response_frame(&mut output, br#"{"ok":true}"#).unwrap();
    assert_eq!(output, b"{\"ok\":true}\n");
}

#[test]
fn setup_receipt_does_not_embed_unbounded_source_diagnostics() {
    let generation = "ab".repeat(32);
    let source_epoch = json!({
        "daemon": {
            "jobs": {
                "core_refresh": {
                    "receipt": {"route_results": "x".repeat(MAX_RESPONSE_BYTES)}
                }
            }
        },
        "lexical": {"generation_id": generation},
    });
    assert!(bounded_value(json!({"status": source_epoch.clone()})).is_err());

    let facts = bounded_setup_facts(
        json!({
            "daemon_requested": true,
            "refresh_request": {"status": "published"},
            "wait": true,
        }),
        None,
        &source_epoch,
    )
    .unwrap();

    assert_eq!(facts["generation_id"], generation);
    assert!(facts.get("status").is_none());
    assert!(canonical(&facts).unwrap().len() < MAX_RESPONSE_BYTES);
}

fn source_unclaimed_terminal_failure(retryable: bool) -> RefreshTerminalOutcome {
    let blocked = SourceRouteIdentity::from_sha256("ab".repeat(32)).unwrap();
    let retryable_route = SourceRouteIdentity::from_sha256("cd".repeat(32)).unwrap();
    RefreshTerminalOutcome::new(
        RefreshOutcomeCode::SourceUnclaimed,
        retryable,
        if retryable {
            BTreeSet::from([blocked.clone(), retryable_route.clone()])
        } else {
            BTreeSet::from([blocked.clone()])
        },
        if retryable {
            BTreeSet::from([retryable_route])
        } else {
            BTreeSet::new()
        },
        BTreeSet::from([blocked]),
        "00000000-0000-0000-0000-000000000123".to_owned(),
        Some("cd".repeat(32)),
        None,
        Some(if retryable {
            RefreshRetryAdvice::RetryRetryableRoutesAndInspectBlocked
        } else {
            RefreshRetryAdvice::InspectSources
        }),
        None,
    )
    .unwrap()
}

fn run_terminal_failure(
    terminal: crate::semantic::SourceBackedRefreshTerminalError,
) -> (ExitCode, Vec<u8>) {
    let root = tempfile::tempdir().unwrap();
    let request = json!({
        "data_root": root.path(),
        "operation": "RefreshAndWait",
        "options": {},
        "protocol_version": CORE_PRO_PROTOCOL_VERSION.get(),
        "schema_version": 1,
    });
    let mut input = canonical(&request).unwrap();
    input.push(b'\n');
    let mut output = Vec::new();
    let error = anyhow::Error::new(terminal);
    let status = capability_exit_code(run_with_io(
        std::io::Cursor::new(input),
        &mut output,
        move |_| -> Result<Value> { Err(error) },
    ));
    (status, output)
}

#[test]
fn refresh_progress_event_is_canonical_bounded_control_free_protocol_json() {
    let status = crate::semantic::RefreshStatus::parse_schema_v1(json!({
        "request_id": "logical-request",
        "request_state": "running",
        "logical_request_id": "logical-request",
        "logical_phase": "exact_successor",
        "physical_attempt_id": "physical-attempt",
        "physical_attempt_state": "running",
        "progress_owner_request_id": "physical-attempt",
        "progress_owner_attempt_state": "running",
        "progress": {
            "phase": "copying",
            "completed_sources": 1,
            "total_sources": 2,
            "total_sources_known": true,
            "current_source": "history\u{001b}[31m\u{009b}red",
            "completed_records": 8,
            "completed_bytes": 256,
            "providers": ["codex"],
            "processed_sessions": 3,
            "processed_messages": 5,
            "processed_tool_calls": 2,
            "processed_bytes": 1024,
            "elapsed_millis": 1200,
            "whole_run_stage": "reading",
            "estimated_remaining_millis": 3400,
            "current_source_progress": {
                "stage": "online_backup",
                "snapshot_pages_completed": 2,
                "snapshot_pages_total": 4,
                "snapshot_bytes_completed": 256,
                "snapshot_bytes_total": 512
            }
        }
    }))
    .unwrap();
    let mut output = Vec::new();
    let mut events = ProtocolEventWriter::new(Operation::CoreSetup, &mut output);

    events.refresh(&status).unwrap();

    assert_eq!(output.iter().filter(|byte| **byte == b'\n').count(), 1);
    assert!(!output.contains(&0x1b));
    assert!(!output.contains(&b'\r'));
    let frame: Value = serde_json::from_slice(output.strip_suffix(b"\n").unwrap()).unwrap();
    assert_eq!(canonical(&frame).unwrap(), output[..output.len() - 1]);
    assert_eq!(frame["type"], "ctx_core_capability_event");
    assert_eq!(frame["event"], "refresh");
    assert_eq!(frame["operation"], "CoreSetup");
    assert_eq!(frame["protocol_version"], CORE_PRO_PROTOCOL_VERSION.get());
    assert_eq!(frame["schema_version"], 1);
    assert_eq!(frame["sequence"], 0);
    assert_eq!(frame["refresh"]["request_id"], "logical-request");
    assert_eq!(frame["refresh"]["request_state"], "running");
    assert_eq!(frame["refresh"]["whole_run_stage"], "reading");
    assert_eq!(frame["refresh"]["providers"], json!(["codex"]));
    assert_eq!(
        frame["refresh"]["current_source"],
        "history\\u{001B}[31m\\u{009B}red"
    );
    assert_eq!(
        frame["refresh"]["current_source_progress"],
        json!({
            "stage": "online_backup",
            "snapshot_pages_completed": 2,
            "snapshot_pages_total": 4,
            "snapshot_bytes_completed": 256,
            "snapshot_bytes_total": 512,
        })
    );
    assert!(frame["refresh"].get("terminal_state").is_none());
}

#[test]
fn refresh_terminal_event_preserves_typed_outcome_and_hides_arbitrary_detail() {
    let route = "ab".repeat(32);
    let retained = "cd".repeat(32);
    let status = crate::semantic::RefreshStatus::parse_schema_v1(json!({
        "request_id": "logical-request",
        "request_state": "failed",
        "logical_request_id": "logical-request",
        "logical_phase": "terminal",
        "physical_attempt_id": "physical-attempt",
        "physical_attempt_state": "failed",
        "progress_owner_request_id": "physical-attempt",
        "progress_owner_attempt_state": "failed",
        "progress": {
            "phase": "failed",
            "completed_sources": 1,
            "total_sources": 2,
            "total_sources_known": true,
            "providers": ["codex"],
            "whole_run_stage": "failed"
        },
        "structured_outcome": {
            "code": "index_corruption",
            "class": "corruption",
            "retryable": false,
            "affected_routes": [route],
            "retryable_routes": [],
            "blocked_routes": [route],
            "physical_attempt_id": "physical-attempt",
            "retained_generation": retained,
            "published_generation": null,
            "retry_advice": "rebuild_index",
            "detail": "raw path, token, and arbitrary diagnostics stay inside Core"
        }
    }))
    .unwrap();
    let mut output = Vec::new();
    let mut events = ProtocolEventWriter::new(Operation::RefreshAndWait, &mut output);

    events.refresh(&status).unwrap();

    let frame: Value = serde_json::from_slice(output.strip_suffix(b"\n").unwrap()).unwrap();
    assert_eq!(frame["event"], "refresh");
    assert_eq!(frame["refresh"]["request_state"], "failed");
    assert_eq!(
        frame["refresh"]["terminal_state"]["error_code"],
        "index_corruption"
    );
    assert_eq!(frame["refresh"]["terminal_state"]["retryable"], false);
    assert_eq!(
        frame["refresh"]["terminal_state"]["details"],
        json!({
            "affected_routes": [route],
            "blocked_routes": [route],
            "class": "corruption",
            "physical_attempt_id": "physical-attempt",
            "retained_generation": retained,
            "retry_advice": "rebuild_index",
            "retryable_routes": [],
        })
    );
    assert!(!String::from_utf8(output)
        .unwrap()
        .contains("arbitrary diagnostics"));
}

#[test]
fn protocol_stream_orders_progress_before_the_single_terminal_response() {
    let root = tempfile::tempdir().unwrap();
    let request = json!({
        "data_root": root.path(),
        "operation": "CoreSetup",
        "options": {
            "defer_fresh_empty_wait": false,
            "no_daemon": false,
            "notice_lines": [],
            "progress": "events",
            "semantic": false,
            "wait": true
        },
        "protocol_version": CORE_PRO_PROTOCOL_VERSION.get(),
        "schema_version": 1,
    });
    let mut input = canonical(&request).unwrap();
    input.push(b'\n');
    let status = crate::semantic::RefreshStatus::parse_schema_v1(json!({
        "request_state": "running",
        "progress": {
            "phase": "reading",
            "completed_sources": 0,
            "total_sources": 1,
            "total_sources_known": true,
            "whole_run_stage": "reading"
        }
    }))
    .unwrap();
    let mut output = Vec::new();

    run_with_protocol_io(
        std::io::Cursor::new(input),
        &mut output,
        |request, events| {
            events.refresh(&status)?;
            Ok(json!({
                "facts": {"generation_id": null},
                "ok": true,
                "operation": request.operation.name(),
                "protocol_version": CORE_PRO_PROTOCOL_VERSION.get(),
                "schema_version": 1,
            }))
        },
    )
    .unwrap();

    let frames = output
        .split(|byte| *byte == b'\n')
        .filter(|frame| !frame.is_empty())
        .map(|frame| serde_json::from_slice::<Value>(frame).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0]["event"], "refresh");
    assert_eq!(frames[0]["sequence"], 0);
    assert_eq!(frames[1]["ok"], true);
    assert!(frames[1].get("type").is_none());
}

#[test]
fn legacy_refresh_request_still_writes_exactly_one_terminal_response() {
    let root = tempfile::tempdir().unwrap();
    let request = json!({
        "data_root": root.path(),
        "operation": "RefreshAndWait",
        "options": {},
        "protocol_version": CORE_PRO_PROTOCOL_VERSION.get(),
        "schema_version": 1,
    });
    let mut input = canonical(&request).unwrap();
    input.push(b'\n');
    let mut output = Vec::new();

    run_with_protocol_io(
        std::io::Cursor::new(input),
        &mut output,
        |request, _events| {
            assert!(matches!(
                request.options,
                Options::Refresh { events: false }
            ));
            Ok(json!({
                "facts": {},
                "generation_id": null,
                "ok": true,
                "operation": request.operation.name(),
                "protocol_version": CORE_PRO_PROTOCOL_VERSION.get(),
                "schema_version": 1,
            }))
        },
    )
    .unwrap();

    assert_eq!(output.iter().filter(|byte| **byte == b'\n').count(), 1);
    let response: Value = serde_json::from_slice(output.strip_suffix(b"\n").unwrap()).unwrap();
    assert_eq!(response["ok"], true);
    assert!(response.get("event").is_none());
}

#[test]
fn event_frame_and_cumulative_stream_bounds_fail_before_writing() {
    let oversized = crate::semantic::RefreshStatus::parse_schema_v1(json!({
        "request_state": "running",
        "progress": {
            "phase": "reading",
            "completed_sources": 0,
            "total_sources": 1,
            "total_sources_known": true,
            "current_source": "x".repeat(48 * 1024),
            "whole_run_stage": "reading"
        }
    }))
    .unwrap();
    let mut oversized_output = Vec::new();
    let mut oversized_writer =
        ProtocolEventWriter::new(Operation::CoreSetup, &mut oversized_output);
    let error = oversized_writer.refresh(&oversized).unwrap_err();
    assert!(progress_events::event_writer_error(&error));
    assert!(oversized_output.is_empty());

    let status = crate::semantic::RefreshStatus::parse_schema_v1(json!({
        "request_state": "running",
        "progress": {
            "phase": "reading",
            "completed_sources": 0,
            "total_sources": 1,
            "total_sources_known": true,
            "whole_run_stage": "reading"
        }
    }))
    .unwrap();
    let mut byte_output = Vec::new();
    let mut byte_writer = ProtocolEventWriter::new(Operation::CoreSetup, &mut byte_output);
    byte_writer.exhaust_byte_budget_for_test();
    let error = byte_writer.refresh(&status).unwrap_err();
    assert!(progress_events::event_writer_error(&error));
    assert!(byte_output.is_empty());

    let mut frame_output = Vec::new();
    let mut frame_writer = ProtocolEventWriter::new(Operation::CoreSetup, &mut frame_output);
    frame_writer.exhaust_frame_budget_for_test();
    let error = frame_writer.refresh(&status).unwrap_err();
    assert!(progress_events::event_writer_error(&error));
    assert!(frame_output.is_empty());
}

#[test]
fn broken_event_stream_is_a_typed_writer_failure() {
    struct BrokenWriter;

    impl std::io::Write for BrokenWriter {
        fn write(&mut self, _buffer: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "injected broken event stream",
            ))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let status = crate::semantic::RefreshStatus::parse_schema_v1(json!({
        "request_state": "running",
        "progress": {
            "phase": "reading",
            "completed_sources": 0,
            "total_sources": 1,
            "total_sources_known": true,
            "whole_run_stage": "reading"
        }
    }))
    .unwrap();
    let mut output = BrokenWriter;
    let mut events = ProtocolEventWriter::new(Operation::CoreSetup, &mut output);

    let error = events.refresh(&status).unwrap_err();

    assert!(progress_events::event_writer_error(&error));
    assert!(should_propagate_setup_refresh_failure(false, &error));
}

#[test]
fn recognized_terminal_failure_writes_one_exact_frame_and_exits_nonzero() {
    let root = tempfile::tempdir().unwrap();
    let request = json!({
        "data_root": root.path(),
        "operation": "RefreshAndWait",
        "options": {},
        "protocol_version": CORE_PRO_PROTOCOL_VERSION.get(),
        "schema_version": 1,
    });
    let mut input = canonical(&request).unwrap();
    input.push(b'\n');

    let route = SourceRouteIdentity::from_sha256("ab".repeat(32)).unwrap();
    let physical_attempt_id = "01234567-89ab-cdef-0123-456789abcdef";
    let retained_generation = "cd".repeat(32);
    let terminal: anyhow::Error = crate::semantic::SourceBackedRefreshTerminalError::from(
        RefreshTerminalOutcome::new(
            RefreshOutcomeCode::IndexCorruption,
            false,
            BTreeSet::from([route.clone()]),
            BTreeSet::new(),
            BTreeSet::from([route]),
            physical_attempt_id.to_owned(),
            Some(retained_generation.clone()),
            None,
            Some(RefreshRetryAdvice::RebuildIndex),
            Some("arbitrary source detail must not cross the boundary".to_owned()),
        )
        .unwrap(),
    )
    .into();
    let terminal = terminal.context("arbitrary internal context must not cross the boundary");
    let mut output = Vec::new();

    let status = capability_exit_code(run_with_io(
        std::io::Cursor::new(input),
        &mut output,
        |request| {
            assert_eq!(request.operation, Operation::RefreshAndWait);
            Err(terminal)
        },
    ));

    assert_eq!(status, ExitCode::FAILURE);
    assert_eq!(output.iter().filter(|byte| **byte == b'\n').count(), 1);
    assert!(!output.contains(&0x1b));
    let response: Value = serde_json::from_slice(output.strip_suffix(b"\n").unwrap()).unwrap();
    assert_eq!(canonical(&response).unwrap(), output[..output.len() - 1]);
    assert_eq!(
        response
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "details",
            "error_code",
            "ok",
            "operation",
            "protocol_version",
            "retryable",
            "schema_version",
        ])
    );
    assert_eq!(
        response["details"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "affected_routes",
            "blocked_routes",
            "class",
            "physical_attempt_id",
            "retained_generation",
            "retry_advice",
            "retryable_routes",
        ])
    );
    assert_eq!(response["error_code"], "index_corruption");
    assert_eq!(response["retryable"], false);
    assert_eq!(
        response["details"]["physical_attempt_id"],
        physical_attempt_id
    );
    assert_eq!(
        response["details"]["retained_generation"],
        retained_generation
    );
    assert!(!String::from_utf8(output).unwrap().contains('\u{009b}'));
}

#[test]
fn malformed_and_unknown_failures_remain_silent_and_nonzero() {
    let mut malformed_output = Vec::new();
    let malformed_status = capability_exit_code(run_with_io(
        std::io::Cursor::new(b"not-json\n"),
        &mut malformed_output,
        |_| -> Result<Value> { panic!("malformed input must not execute") },
    ));
    assert_eq!(malformed_status, ExitCode::FAILURE);
    assert!(malformed_output.is_empty());

    let root = tempfile::tempdir().unwrap();
    let request = json!({
        "data_root": root.path(),
        "operation": "RefreshAndWait",
        "options": {},
        "protocol_version": CORE_PRO_PROTOCOL_VERSION.get(),
        "schema_version": 1,
    });
    let mut input = canonical(&request).unwrap();
    input.push(b'\n');
    let mut internal_output = Vec::new();
    let internal_status = capability_exit_code(run_with_io(
        std::io::Cursor::new(input),
        &mut internal_output,
        |_| Err(anyhow!("unrecognized internal failure")),
    ));
    assert_eq!(internal_status, ExitCode::FAILURE);
    assert!(internal_output.is_empty());
}

#[test]
fn local_usage_summary_returns_canonical_config_error_without_aborting() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("config.toml"),
        "[local_usage]\nenabled = unavailable\n",
    )
    .unwrap();

    let response = execute(
        Request {
            data_root: root.path().to_path_buf(),
            operation: Operation::LocalUsageSummary,
            options: Options::Empty,
        },
        &mut IgnoreEvents,
    )
    .unwrap();

    assert_eq!(response["ok"], true);
    assert_eq!(response["operation"], "LocalUsageSummary");
    assert_eq!(
        response["facts"],
        serde_json::to_value(crate::local_usage::UsageReport::config_error()).unwrap()
    );
    assert!(!root.path().join("usage.sqlite").exists());
}

#[test]
fn local_usage_summary_protocol_version_mismatches_remain_hard_failures() {
    let root = tempfile::tempdir().unwrap();
    let request = json!({
        "data_root": root.path(),
        "operation": "LocalUsageSummary",
        "options": {},
        "protocol_version": CORE_PRO_PROTOCOL_VERSION.get(),
        "schema_version": 1,
    });
    assert!(parse_frame(canonical(&request).unwrap()).is_ok());

    let mut wrong_protocol = request.clone();
    wrong_protocol["protocol_version"] = json!(CORE_PRO_PROTOCOL_VERSION.get() + 1);
    assert!(parse_frame(canonical(&wrong_protocol).unwrap()).is_err());

    let mut wrong_schema = request.clone();
    wrong_schema["schema_version"] = json!(2);
    assert!(parse_frame(canonical(&wrong_schema).unwrap()).is_err());

    let mut unknown_field = request;
    unknown_field["unexpected"] = json!(true);
    assert!(parse_frame(canonical(&unknown_field).unwrap()).is_err());
}

#[test]
fn canonical_response_is_bounded_machine_json() {
    let bytes = canonical(&json!({"schema_version": 1, "ok": true})).unwrap();
    assert!(bytes.len() <= MAX_RESPONSE_BYTES);
    assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap()["ok"], true);
    assert!(!bytes.contains(&b'\n'));
}

#[test]
fn managed_setup_generation_is_optional_and_prefers_publication() {
    let no_generation = json!({"lexical": {"generation_id": null}});
    assert_eq!(setup_generation_id(None, &no_generation), None);

    let current = "1".repeat(64);
    let status = json!({"lexical": {"generation_id": current}});
    assert_eq!(setup_generation_id(None, &status), Some("1".repeat(64)));
    assert_eq!(
        setup_generation_id(Some("2".repeat(64)), &status),
        Some("2".repeat(64))
    );
}

#[test]
fn managed_fresh_default_preserves_core_only_empty_publication_wait() {
    let empty: anyhow::Error = crate::semantic::SourceBackedRefreshPendingPublication::new(
        "fresh-empty".to_owned(),
        "queued".to_owned(),
        0,
    )
    .into();
    let nonempty: anyhow::Error = crate::semantic::SourceBackedRefreshPendingPublication::new(
        "fresh-nonempty".to_owned(),
        "queued".to_owned(),
        1,
    )
    .into();
    assert!(should_wait_for_fresh_empty_publication(false, &empty));
    assert!(!should_wait_for_fresh_empty_publication(true, &empty));
    assert!(!should_wait_for_fresh_empty_publication(false, &nonempty));
}

#[test]
fn managed_waited_refresh_failure_is_not_reported_as_setup_success() {
    let failure = anyhow!("source refresh failed");
    assert!(should_propagate_setup_refresh_failure(true, &failure));
    assert!(!should_propagate_setup_refresh_failure(false, &failure));
}

#[test]
fn managed_setup_presentation_options_are_closed_and_bounded() {
    let options = json!({
        "defer_fresh_empty_wait": true,
        "no_daemon": false,
        "notice_lines": ["approved line", "", "https://companion.example.test/opaque"],
        "progress": "auto",
        "semantic": false,
        "wait": false,
    });
    let parsed = parse_options(Operation::CoreSetup, &options).unwrap();
    let Options::Setup(CoreSetupOptions {
        defer_fresh_empty_wait,
        notice_lines,
        progress,
        ..
    }) = parsed
    else {
        panic!("expected setup options")
    };
    assert!(defer_fresh_empty_wait);
    assert_eq!(notice_lines[2], "https://companion.example.test/opaque");
    assert_eq!(
        progress,
        SetupProgressMode::Legacy(crate::progress::ProgressArg::Auto)
    );

    let mut event_options = options.clone();
    event_options["progress"] = json!("events");
    let Options::Setup(CoreSetupOptions { progress, .. }) =
        parse_options(Operation::CoreSetup, &event_options).unwrap()
    else {
        panic!("expected setup event options")
    };
    assert_eq!(progress, SetupProgressMode::Events);

    assert!(matches!(
        parse_options(Operation::RefreshAndWait, &json!({})).unwrap(),
        Options::Refresh { events: false }
    ));
    assert!(matches!(
        parse_options(Operation::RefreshAndWait, &json!({"progress": "events"})).unwrap(),
        Options::Refresh { events: true }
    ));
    assert!(parse_options(Operation::RefreshAndWait, &json!({"progress": "plain"})).is_err());

    let mut invalid = options.clone();
    invalid["notice_lines"] = json!(["line\nforgery"]);
    assert!(parse_options(Operation::CoreSetup, &invalid).is_err());
    invalid = options;
    invalid["progress"] = json!("verbose");
    assert!(parse_options(Operation::CoreSetup, &invalid).is_err());

    let mut oversized = json!({
        "defer_fresh_empty_wait": true,
        "no_daemon": false,
        "notice_lines": ["x".repeat(513)],
        "progress": "auto",
        "semantic": false,
        "wait": false,
    });
    assert!(parse_options(Operation::CoreSetup, &oversized).is_err());
    oversized["notice_lines"] = json!(["x".repeat(512)]);
    assert!(parse_options(Operation::CoreSetup, &oversized).is_ok());
}

#[test]
fn oversized_live_notice_degrades_to_plain_progress_before_cursor_rendering() {
    let lines = vec!["one line wider than a narrow terminal".to_owned()];
    assert_eq!(
        progress_mode_for_notice(crate::progress::ProgressArg::Auto, Some(32), &lines),
        crate::progress::ProgressArg::Plain
    );
    assert_eq!(
        progress_mode_for_notice(crate::progress::ProgressArg::Auto, Some(80), &lines),
        crate::progress::ProgressArg::Auto
    );
    assert_eq!(
        progress_mode_for_notice(crate::progress::ProgressArg::Plain, Some(32), &lines),
        crate::progress::ProgressArg::Plain
    );
}
