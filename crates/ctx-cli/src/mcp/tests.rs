use std::{
    env,
    ffi::OsString,
    io::{Cursor, Error, ErrorKind},
    sync::{Arc, Mutex, MutexGuard},
};

use ctx_history_core::platform_security::restrict_private_directory;

use super::*;

#[derive(Clone, Copy)]
enum OutputFailure {
    None,
    Write,
    Flush,
}

struct TracedWriter {
    failure: OutputFailure,
    trace: Arc<Mutex<Vec<&'static str>>>,
}

impl Write for TracedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if matches!(self.failure, OutputFailure::Write) {
            self.trace.lock().unwrap().push("write_failed");
            return Err(Error::new(ErrorKind::BrokenPipe, "test write failure"));
        }
        self.trace.lock().unwrap().push("write");
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if matches!(self.failure, OutputFailure::Flush) {
            self.trace.lock().unwrap().push("flush_failed");
            return Err(Error::new(ErrorKind::BrokenPipe, "test flush failure"));
        }
        self.trace.lock().unwrap().push("flush");
        Ok(())
    }
}

struct LocalUsageEnvGuard {
    _lock: MutexGuard<'static, ()>,
    saved: Option<OsString>,
}

impl LocalUsageEnvGuard {
    fn unset() -> Self {
        let lock = crate::config::TEST_LOCAL_USAGE_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let saved = env::var_os("CTX_LOCAL_USAGE_ENABLED");
        env::remove_var("CTX_LOCAL_USAGE_ENABLED");
        Self { _lock: lock, saved }
    }
}

impl Drop for LocalUsageEnvGuard {
    fn drop(&mut self) {
        match &self.saved {
            Some(value) => env::set_var("CTX_LOCAL_USAGE_ENABLED", value),
            None => env::remove_var("CTX_LOCAL_USAGE_ENABLED"),
        }
    }
}

#[test]
fn sql_is_neither_advertised_nor_handled_as_an_mcp_tool() {
    let temp = tempfile::tempdir().unwrap();
    let tool_names = tool_definitions()
        .into_iter()
        .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    assert!(
        !tool_names.iter().any(|name| name == "sql"),
        "{tool_names:?}"
    );

    let (handled, usage) = handle_tools_call(
        json!({
            "name": "sql",
            "arguments": {"sql": "SELECT 1 AS one"},
        }),
        temp.path(),
    );
    let error = match handled {
        Ok(_) => panic!("removed SQL tool must be rejected"),
        Err(error) => error,
    };

    assert_eq!(error["code"], -32602);
    assert!(error["data"]["error"]
        .as_str()
        .unwrap()
        .contains("unknown tool sql"));
    assert!(usage.is_none());
    assert!(
        std::fs::read_dir(temp.path()).unwrap().next().is_none(),
        "an unknown MCP tool must not initialize the data root"
    );
}

#[test]
fn query_events_is_advertised_as_one_read_only_bounded_page_with_canonical_args() {
    let tool = tool_definitions()
        .into_iter()
        .find(|tool| tool["name"] == "query_events")
        .expect("query_events tool definition");
    assert_eq!(tool["annotations"]["readOnlyHint"], true);
    let properties = tool["inputSchema"]["properties"].as_object().unwrap();
    for canonical in ["parent_session", "root_session", "limit", "content"] {
        assert!(properties.contains_key(canonical), "missing {canonical}");
    }
    for removed in [
        "parent",
        "root",
        "max_items",
        "page_items",
        "max_bytes",
        "byte_budget",
    ] {
        assert!(!properties.contains_key(removed));
    }
    assert_eq!(properties["limit"]["default"], 10_000);
}

#[test]
fn query_events_rejects_removed_aliases_and_page_budget_arguments() {
    let temp = tempfile::tempdir().unwrap();
    for removed in [
        "parent",
        "root",
        "max_items",
        "page_items",
        "max_bytes",
        "byte_budget",
    ] {
        let (handled, _) = handle_tools_call(
            json!({"name": "query_events", "arguments": {(removed): 1}}),
            temp.path(),
        );
        let handled = handled.unwrap().value;
        assert_eq!(handled["isError"], true, "accepted {removed}");
        assert_eq!(
            handled["structuredContent"]["error_code"],
            "invalid_request"
        );
    }
}

#[test]
fn search_content_scope_schema_and_runtime_are_in_parity() {
    let tool = tool_definitions()
        .into_iter()
        .find(|tool| tool["name"] == "search")
        .expect("search tool definition");
    let content_scope = &tool["inputSchema"]["properties"]["content_scope"];
    assert_eq!(
        content_scope["enum"],
        json!(["all", "transcript", "calls", "outputs"])
    );
    assert_eq!(content_scope["default"], "all");
    assert_eq!(
        tool["inputSchema"]["not"]["required"],
        json!(["content_scope", "event_type"])
    );

    for value in ["all", "transcript", "calls", "outputs"] {
        let request = search_request(&json!({
            "query": "onboarding",
            "content_scope": value,
        }))
        .unwrap();
        assert!(
            matches!(
                (value, &request.content_scope),
                ("all", SearchContentScope::All)
                    | ("transcript", SearchContentScope::Transcript)
                    | ("calls", SearchContentScope::Calls)
                    | ("outputs", SearchContentScope::Outputs)
            ),
            "runtime mapping for {value} did not match the advertised schema"
        );
    }

    for invalid in ["All", "TRANSCRIPT", "call", "outputs "] {
        let error = search_request(&json!({
            "query": "onboarding",
            "content_scope": invalid,
        }))
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("content_scope must be one of all, transcript, calls, outputs"),
            "{invalid}: {error:#}"
        );
    }
}

#[test]
fn search_content_scope_defaults_to_all_in_the_forwarded_request() {
    let request = search_request(&json!({"query": "onboarding"})).unwrap();
    assert!(matches!(request.content_scope, SearchContentScope::All));
}

#[test]
fn search_content_scope_and_event_type_conflict_before_filesystem_work() {
    let temp = tempfile::tempdir().unwrap();
    let (handled, _) = handle_tools_call(
        json!({
            "name": "search",
            "arguments": {
                "query": "onboarding",
                "content_scope": "all",
                "event_type": "message"
            }
        }),
        temp.path(),
    );
    let handled = handled.unwrap().value;
    assert_eq!(handled["isError"], true);
    assert_eq!(
        handled["structuredContent"]["error_code"],
        "invalid_request"
    );
    assert!(handled["structuredContent"]["error"]
        .as_str()
        .unwrap()
        .contains("content_scope and event_type are mutually exclusive"));
    assert!(
        std::fs::read_dir(temp.path()).unwrap().next().is_none(),
        "the conflicting MCP request must fail before search or storage work"
    );
}

#[test]
fn semantic_only_content_scope_rejects_before_daemon_or_filesystem_work() {
    let temp = tempfile::tempdir().unwrap();
    let (handled, _) = handle_tools_call(
        json!({
            "name": "search",
            "arguments": {
                "query": "onboarding",
                "backend": "semantic",
                "content_scope": "outputs"
            }
        }),
        temp.path(),
    );
    let handled = handled.unwrap().value;
    assert_eq!(handled["isError"], true);
    assert!(handled["structuredContent"]["error"]
        .as_str()
        .unwrap()
        .contains("semantic retrieval does not support content scope 'outputs'"));
    assert!(
        std::fs::read_dir(temp.path()).unwrap().next().is_none(),
        "unsupported semantic-only MCP input must fail before daemon or storage work"
    );
}

fn run_one_status_response(
    failure: OutputFailure,
) -> (
    std::result::Result<(), McpServeFailure>,
    Vec<&'static str>,
    bool,
) {
    let root = tempfile::tempdir().unwrap();
    restrict_private_directory(root.path()).unwrap();
    std::fs::write(
        root.path().join("config.toml"),
        "[analytics]\nenabled = false\n[local_usage]\nenabled = true\n",
    )
    .unwrap();
    let request = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": "status",
        "method": "tools/call",
        "params": {"name": "status", "arguments": {}}
    }))
    .unwrap();
    let mut input = Cursor::new([request, vec![b'\n']].concat());
    let trace = Arc::new(Mutex::new(Vec::new()));
    let mut output = TracedWriter {
        failure,
        trace: trace.clone(),
    };
    let mut initialized = true;
    let mut telemetry = McpTelemetry::start(root.path().to_path_buf());
    let mut usage = McpUsageRecorder::start(root.path().to_path_buf());
    usage.set_test_trace(trace.clone());

    let result = serve_stdio_loop(
        root.path(),
        &mut input,
        &mut output,
        &mut initialized,
        &mut telemetry,
        &mut usage,
    );
    let recorded = root.path().join("usage.sqlite").exists();
    let trace = trace.lock().unwrap().clone();
    (result, trace, recorded)
}

#[test]
fn local_usage_commit_occurs_once_after_flush_and_never_after_output_failure() {
    let _env = LocalUsageEnvGuard::unset();

    let (delivered, trace, recorded) = run_one_status_response(OutputFailure::None);
    assert!(delivered.is_ok());
    assert!(recorded);
    assert_eq!(
        trace
            .iter()
            .filter(|entry| **entry == "local_usage")
            .count(),
        1
    );
    let flushed_at = trace.iter().position(|entry| *entry == "flush").unwrap();
    let recorded_at = trace
        .iter()
        .position(|entry| *entry == "local_usage")
        .unwrap();
    assert!(flushed_at < recorded_at, "{trace:?}");

    let (write_failed, trace, recorded) = run_one_status_response(OutputFailure::Write);
    assert!(matches!(
        write_failed.unwrap_err().reason,
        McpStopReasonV1::StdoutWriteError
    ));
    assert!(!recorded);
    assert!(!trace.contains(&"local_usage"));

    let (flush_failed, trace, recorded) = run_one_status_response(OutputFailure::Flush);
    assert!(matches!(
        flush_failed.unwrap_err().reason,
        McpStopReasonV1::StdoutFlushError
    ));
    assert!(!recorded);
    assert!(!trace.contains(&"local_usage"));
}
