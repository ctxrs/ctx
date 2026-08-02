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
