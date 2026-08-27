use super::*;
#[cfg(unix)]
use std::path::PathBuf;

#[path = "query_service_transport_tests/admission_lifecycle.rs"]
mod admission_lifecycle;

#[cfg(unix)]
use std::io::Read as _;

#[cfg(any(unix, windows))]
const TEST_QUERY_REQUEST_READ_TIMEOUT: StdDuration = StdDuration::from_millis(100);

#[cfg(unix)]
fn short_test_query_socket_path() -> Result<(tempfile::TempDir, PathBuf)> {
    let socket_root = tempfile::Builder::new()
        .prefix("ctx-query-test-")
        .tempdir_in("/tmp")?;
    let socket_path = socket_root.path().join("q.sock");
    if socket_path.as_os_str().as_bytes().len() > DAEMON_QUERY_SOCKET_PATH_SAFE_BYTES {
        return Err(anyhow!("test daemon query socket path is too long"));
    }
    Ok((socket_root, socket_path))
}

#[cfg(any(unix, windows))]
fn start_test_query_service(data_root: &Path) -> Result<DaemonQueryService> {
    let wakeup = Arc::new(super::daemon_wakeup::DaemonWakeup::default());
    let source_refresh = Arc::new(super::source_backed_refresh_adapter::refresh_engine(
        &crate::test_support::CONFIG,
    ));
    let handler = ctx_authenticated_request_handler(
        data_root,
        SharedSemanticRuntime::default(),
        source_refresh,
        wakeup,
        &crate::test_support::CONFIG,
    );
    start_daemon_query_service_with_request_timeout(
        data_root,
        handler,
        TEST_QUERY_REQUEST_READ_TIMEOUT,
    )
}

#[cfg(any(unix, windows))]
fn start_test_source_refresh_service(
    data_root: &Path,
) -> Result<(
    DaemonQueryService,
    Arc<super::source_backed_refresh_coordinator::CoreRefreshEngine>,
)> {
    let wakeup = Arc::new(super::daemon_wakeup::DaemonWakeup::default());
    let source_refresh = Arc::new(super::source_backed_refresh_adapter::refresh_engine(
        &crate::test_support::CONFIG,
    ));
    let handler = ctx_authenticated_request_handler(
        data_root,
        SharedSemanticRuntime::default(),
        Arc::clone(&source_refresh),
        wakeup,
        &crate::test_support::CONFIG,
    );
    let service = start_daemon_source_refresh_service_with_request_timeout(
        data_root,
        handler,
        TEST_QUERY_REQUEST_READ_TIMEOUT,
    )?;
    Ok((service, source_refresh))
}

#[cfg(any(unix, windows))]
fn wait_for_active_query(service: &DaemonQueryService) -> Result<()> {
    let started = Instant::now();
    while started.elapsed() < StdDuration::from_secs(2) {
        if service.activity.snapshot().0 == 1 {
            return Ok(());
        }
        std::thread::sleep(StdDuration::from_millis(5));
    }
    Err(anyhow!(
        "daemon query service did not accept the test client"
    ))
}

struct CountingAfterWrite(Arc<std::sync::atomic::AtomicUsize>);

impl CountingAfterWrite {
    fn run(&self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[derive(Default)]
enum CountingPostWriteAction<'a> {
    #[default]
    None,
    Completion(&'a CountingAfterWrite),
}

impl PostWriteAction for CountingPostWriteAction<'_> {
    fn run(self) {
        if let Self::Completion(completion) = self {
            completion.run();
        }
    }
}

struct CountingAuthenticatedHandler {
    handled: Arc<std::sync::atomic::AtomicUsize>,
    after_write: CountingAfterWrite,
    fail: bool,
}

impl AuthenticatedRequestHandler for CountingAuthenticatedHandler {
    type PostWriteAction<'a> = CountingPostWriteAction<'a>;

    fn handle<'a>(
        &'a self,
        _service: &ServiceId,
        _request: AuthenticatedRequest,
    ) -> HandlerOutcome<Self::PostWriteAction<'a>> {
        self.handled
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let response = if self.fail {
            Err(anyhow!("authenticated handler failed"))
        } else {
            Ok(json!({"ok": true, "schema_version": 1}))
        };
        HandlerOutcome::with_post_write_action(
            response,
            CountingPostWriteAction::Completion(&self.after_write),
        )
    }
}

struct DisconnectedResponseWriter;

impl std::io::Write for DisconnectedResponseWriter {
    fn write(&mut self, _buffer: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "test client disconnected",
        ))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn authenticated_transport_finalizes_once_after_success_error_and_disconnect() -> Result<()> {
    const TOKEN: &str = "0123456789abcdef0123456789abcdef";
    let request = || Ok(format!("{{\"token\":\"{TOKEN}\",\"op\":\"ping\"}}"));

    for (fail, disconnected) in [(false, false), (true, false), (false, true)] {
        let handled = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let finalized = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let handler = CountingAuthenticatedHandler {
            handled: Arc::clone(&handled),
            after_write: CountingAfterWrite(Arc::clone(&finalized)),
            fail,
        };
        if disconnected {
            assert!(handle_authenticated_daemon_stream(
                &handler,
                &ServiceId::new("test-service")?,
                TOKEN,
                DisconnectedResponseWriter,
                request(),
            )
            .is_err());
        } else {
            let mut response = Vec::new();
            handle_authenticated_daemon_stream(
                &handler,
                &ServiceId::new("test-service")?,
                TOKEN,
                &mut response,
                request(),
            )?;
            let expected = if fail {
                b"{\"error\":\"authenticated handler failed\",\"ok\":false}\n".as_slice()
            } else {
                b"{\"ok\":true,\"schema_version\":1}\n".as_slice()
            };
            assert_eq!(response, expected);
        }
        assert_eq!(handled.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(finalized.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
    Ok(())
}

#[test]
fn malformed_and_bad_token_requests_never_reach_handler_or_post_write_action() -> Result<()> {
    const TOKEN: &str = "0123456789abcdef0123456789abcdef";
    let handled = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let finalized = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let handler = CountingAuthenticatedHandler {
        handled: Arc::clone(&handled),
        after_write: CountingAfterWrite(Arc::clone(&finalized)),
        fail: false,
    };

    for (request, expected) in [
        ("{not-json", None),
        (
            "{\"token\":\"wrong\",\"op\":\"ping\"}",
            Some(b"{\"error\":\"daemon query authentication failed\",\"ok\":false}\n".as_slice()),
        ),
    ] {
        let mut response = Vec::new();
        handle_authenticated_daemon_stream(
            &handler,
            &ServiceId::new("test-service")?,
            TOKEN,
            &mut response,
            Ok(request.to_owned()),
        )?;
        if let Some(expected) = expected {
            assert_eq!(response, expected);
        } else {
            assert_eq!(response.last(), Some(&b'\n'));
            let response: Value = serde_json::from_slice(&response)?;
            assert_eq!(response["ok"], false);
        }
    }
    assert_eq!(handled.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(finalized.load(std::sync::atomic::Ordering::SeqCst), 0);
    Ok(())
}

struct RecordingServiceHandler(std::sync::Mutex<Vec<String>>);

impl AuthenticatedRequestHandler for RecordingServiceHandler {
    type PostWriteAction<'a> = NoPostWriteAction;

    fn handle<'a>(
        &'a self,
        service: &ServiceId,
        _request: AuthenticatedRequest,
    ) -> HandlerOutcome<Self::PostWriteAction<'a>> {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(service.as_str().to_owned());
        HandlerOutcome::response(Ok(json!({"ok": true})))
    }
}

struct ParsedRequestHandler(std::sync::Mutex<Vec<Value>>);

impl AuthenticatedRequestHandler for ParsedRequestHandler {
    type PostWriteAction<'a> = NoPostWriteAction;

    fn handle<'a>(
        &'a self,
        _service: &ServiceId,
        request: AuthenticatedRequest,
    ) -> HandlerOutcome<Self::PostWriteAction<'a>> {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(request.into_value());
        HandlerOutcome::response(Ok(json!({"ok": true})))
    }
}

#[test]
fn no_op_post_write_action_is_zero_sized_and_inline() {
    assert_eq!(std::mem::size_of::<NoPostWriteAction>(), 0);
    let outcome = HandlerOutcome::<NoPostWriteAction>::response(Ok(json!({"ok": true})));
    assert_eq!(std::mem::size_of_val(&outcome.after_write_action), 0);
}

#[test]
fn bounded_authenticated_request_is_parsed_once_before_dispatch() -> Result<()> {
    const TOKEN: &str = "0123456789abcdef0123456789abcdef";
    let payload = "x".repeat(DAEMON_QUERY_REQUEST_MAX_BYTES / 2);
    let request = json!({
        "token": TOKEN,
        "op": "ping",
        "payload": payload,
    })
    .to_string();
    assert!(request.len() < DAEMON_QUERY_REQUEST_MAX_BYTES);

    let handler = ParsedRequestHandler(std::sync::Mutex::new(Vec::new()));
    let mut response = Vec::new();
    handle_authenticated_daemon_stream(
        &handler,
        &ServiceId::new("test-service")?,
        TOKEN,
        &mut response,
        Ok(request),
    )?;

    assert_eq!(response, b"{\"ok\":true}\n");
    let requests = handler.0.lock().unwrap_or_else(|error| error.into_inner());
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["op"], "ping");
    assert_eq!(
        requests[0]["payload"].as_str().map(str::len),
        Some(DAEMON_QUERY_REQUEST_MAX_BYTES / 2)
    );
    Ok(())
}

#[test]
fn runtime_service_identity_is_validated_and_forwarded_without_product_mapping() -> Result<()> {
    for invalid in [
        "",
        "UPPER",
        "leading/slash",
        "-leading",
        "trailing-",
        "service_name",
    ] {
        assert!(ServiceId::new(invalid).is_err(), "accepted `{invalid}`");
    }
    assert!(ServiceId::new("a".repeat(65)).is_err());

    let service_id = ServiceId::new("opaque-service-7")?;
    #[cfg(unix)]
    {
        let constructor: fn(ServiceId, PathBuf, bool) -> Result<IpcServiceSpec> =
            IpcServiceSpec::new;
        assert!(constructor(service_id.clone(), PathBuf::from("/"), false,).is_err());
        let spec = constructor(service_id.clone(), PathBuf::from("/tmp/opaque.sock"), false)?;
        assert_eq!(spec.unix_socket_path(), Path::new("/tmp/opaque.sock"));
    }
    #[cfg(not(unix))]
    {
        let spec = IpcServiceSpec::new(service_id.clone(), false)?;
        assert_eq!(spec.service_id(), &service_id);
    }

    let handler = RecordingServiceHandler(std::sync::Mutex::new(Vec::new()));
    let mut response = Vec::new();
    handle_authenticated_daemon_stream(
        &handler,
        &service_id,
        "token",
        &mut response,
        Ok("{\"token\":\"token\",\"op\":\"opaque\"}".to_owned()),
    )?;
    assert_eq!(
        *handler.0.lock().unwrap_or_else(|error| error.into_inner()),
        ["opaque-service-7"]
    );
    assert_eq!(response, b"{\"ok\":true}\n");
    Ok(())
}

#[cfg(unix)]
fn connect_stalled_query_client(data_root: &Path) -> Result<UnixStream> {
    let endpoint = read_daemon_query_endpoint(data_root)?.expect("query endpoint");
    let DaemonQueryEndpoint::Unix { path, .. } = endpoint;
    UnixStream::connect(&path)
        .with_context(|| format!("connect test query socket {}", path.display()))
}

#[cfg(unix)]
fn connect_valid_nonreading_query_client(data_root: &Path) -> Result<UnixStream> {
    let endpoint = read_daemon_query_endpoint(data_root)?.expect("query endpoint");
    let DaemonQueryEndpoint::Unix { path, token } = endpoint;
    let mut stream = UnixStream::connect(&path)
        .with_context(|| format!("connect test query socket {}", path.display()))?;
    writeln!(
        stream,
        "{}",
        serde_json::to_string(&compact_json(json!({
            "schema_version": 1,
            "op": "ping",
            "token": token,
        })))?
    )?;
    Ok(stream)
}

#[cfg(unix)]
#[test]
fn configured_unix_query_stream_drains_response_larger_than_socket_buffer() -> Result<()> {
    use std::io::{Read, Write};

    let (mut server, mut client) = UnixStream::pair()?;
    server.set_nonblocking(true)?;
    configure_daemon_query_stream_unix(&server, StdDuration::from_secs(2))?;
    let response = vec![b'x'; 1024 * 1024];
    let expected = response.len();
    let writer = std::thread::spawn(move || -> std::io::Result<()> {
        server.write_all(&response)?;
        server.shutdown(Shutdown::Write)
    });
    std::thread::sleep(StdDuration::from_millis(25));
    let mut received = Vec::new();
    client.read_to_end(&mut received)?;
    writer.join().expect("query response writer panicked")?;
    assert_eq!(received.len(), expected);
    Ok(())
}

#[cfg(unix)]
#[test]
fn concurrent_unix_roundtrips_wait_for_readiness_and_collect_partial_responses() -> Result<()> {
    const CLIENTS: usize = 24;

    let (_socket_root, socket_path) = short_test_query_socket_path()?;
    let listener = std::os::unix::net::UnixListener::bind(&socket_path)?;
    let server = std::thread::spawn(move || -> Result<()> {
        let mut handlers = Vec::with_capacity(CLIENTS);
        for _ in 0..CLIENTS {
            let (mut stream, _) = listener.accept()?;
            handlers.push(std::thread::spawn(move || -> Result<()> {
                let mut request = Vec::new();
                stream.read_to_end(&mut request)?;
                assert_eq!(request, b"{\"op\":\"ping\"}\n");
                std::thread::sleep(StdDuration::from_millis(25));
                stream.write_all(b"{\"ok\":")?;
                std::thread::sleep(StdDuration::from_millis(5));
                stream.write_all(b"true}\n")?;
                stream.shutdown(Shutdown::Write)?;
                Ok(())
            }));
        }
        for handler in handlers {
            handler.join().expect("response handler panicked")?;
        }
        Ok(())
    });

    let endpoint = DaemonQueryEndpoint::Unix {
        path: socket_path,
        token: "0123456789abcdef0123456789abcdef".to_owned(),
    };
    let barrier = Arc::new(std::sync::Barrier::new(CLIENTS + 1));
    let mut clients = Vec::with_capacity(CLIENTS);
    for _ in 0..CLIENTS {
        let endpoint = endpoint.clone();
        let barrier = barrier.clone();
        clients.push(std::thread::spawn(move || -> Result<()> {
            barrier.wait();
            let response = daemon_query_roundtrip(
                &endpoint,
                b"{\"op\":\"ping\"}\n",
                StdDuration::from_secs(2),
                1024,
            )?;
            assert_eq!(response, "{\"ok\":true}\n");
            Ok(())
        }));
    }
    barrier.wait();
    for client in clients {
        client.join().expect("query client panicked")?;
    }
    server.join().expect("query server panicked")?;
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn unix_connect_obeys_the_roundtrip_deadline_when_the_accept_queue_is_full() -> Result<()> {
    use std::os::fd::AsRawFd as _;

    let (_socket_root, socket_path) = short_test_query_socket_path()?;
    let listener = std::os::unix::net::UnixListener::bind(&socket_path)?;
    if unsafe { libc::listen(listener.as_raw_fd(), 1) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // Linux admits backlog + 1 established AF_UNIX clients. With neither
    // accepted, a third nonblocking connect must remain EAGAIN until its one
    // caller-provided deadline expires.
    let _queued_one = UnixStream::connect(&socket_path)?;
    let _queued_two = UnixStream::connect(&socket_path)?;
    let endpoint = DaemonQueryEndpoint::Unix {
        path: socket_path,
        token: "0123456789abcdef0123456789abcdef".to_owned(),
    };

    let timeout = StdDuration::from_millis(80);
    let started = Instant::now();
    let error = daemon_query_roundtrip(&endpoint, b"{}\n", timeout, 1024)
        .expect_err("a full non-accepting socket queue must time out");
    let elapsed = started.elapsed();

    assert_eq!(
        error
            .downcast_ref::<std::io::Error>()
            .map(std::io::Error::kind),
        Some(std::io::ErrorKind::TimedOut),
        "{error:#}"
    );
    assert!(elapsed >= StdDuration::from_millis(40), "{elapsed:?}");
    assert!(elapsed < StdDuration::from_millis(400), "{elapsed:?}");
    drop(listener);
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn unix_connect_and_response_read_share_one_absolute_deadline() -> Result<()> {
    use std::os::fd::AsRawFd as _;

    let (_socket_root, socket_path) = short_test_query_socket_path()?;
    let listener = std::os::unix::net::UnixListener::bind(&socket_path)?;
    if unsafe { libc::listen(listener.as_raw_fd(), 1) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let queued_one = UnixStream::connect(&socket_path)?;
    let queued_two = UnixStream::connect(&socket_path)?;
    let server = std::thread::spawn(move || -> Result<()> {
        std::thread::sleep(StdDuration::from_millis(35));
        drop(listener.accept()?.0);
        drop(listener.accept()?.0);
        let (mut stream, _) = listener.accept()?;
        let mut request = Vec::new();
        stream.read_to_end(&mut request)?;
        assert_eq!(request, b"{}\n");
        std::thread::sleep(StdDuration::from_millis(55));
        let _ = stream.write_all(b"{}\n");
        Ok(())
    });
    let endpoint = DaemonQueryEndpoint::Unix {
        path: socket_path,
        token: "0123456789abcdef0123456789abcdef".to_owned(),
    };

    let started = Instant::now();
    let error = daemon_query_roundtrip(&endpoint, b"{}\n", StdDuration::from_millis(70), 1024)
        .expect_err("connect and response phases must not receive separate budgets");
    let elapsed = started.elapsed();

    assert_eq!(
        error
            .downcast_ref::<std::io::Error>()
            .map(std::io::Error::kind),
        Some(std::io::ErrorKind::TimedOut),
        "{error:#}"
    );
    assert!(elapsed < StdDuration::from_millis(350), "{elapsed:?}");
    drop(queued_one);
    drop(queued_two);
    server.join().expect("query server panicked")?;
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn unix_request_write_obeys_the_same_absolute_deadline() -> Result<()> {
    let (_socket_root, socket_path) = short_test_query_socket_path()?;
    let listener = std::os::unix::net::UnixListener::bind(&socket_path)?;
    let server = std::thread::spawn(move || -> Result<()> {
        let (_stream, _) = listener.accept()?;
        std::thread::sleep(StdDuration::from_millis(180));
        Ok(())
    });
    let endpoint = DaemonQueryEndpoint::Unix {
        path: socket_path,
        token: "0123456789abcdef0123456789abcdef".to_owned(),
    };
    let request = vec![b'x'; 8 * 1024 * 1024];

    let started = Instant::now();
    let error = daemon_query_roundtrip(&endpoint, &request, StdDuration::from_millis(60), 1024)
        .expect_err("a non-reading peer must not receive an unbounded request write");
    let elapsed = started.elapsed();

    assert_eq!(
        error
            .downcast_ref::<std::io::Error>()
            .map(std::io::Error::kind),
        Some(std::io::ErrorKind::TimedOut),
        "{error:#}"
    );
    assert!(elapsed < StdDuration::from_millis(350), "{elapsed:?}");
    server.join().expect("query server panicked")?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn partial_unix_response_obeys_one_aggregate_read_deadline() -> Result<()> {
    let (_socket_root, socket_path) = short_test_query_socket_path()?;
    let listener = std::os::unix::net::UnixListener::bind(&socket_path)?;
    let server = std::thread::spawn(move || -> Result<()> {
        let (mut stream, _) = listener.accept()?;
        let mut request = Vec::new();
        stream.read_to_end(&mut request)?;
        for _ in 0..20 {
            std::thread::sleep(StdDuration::from_millis(10));
            if stream.write_all(b"x").is_err() {
                break;
            }
        }
        Ok(())
    });
    let endpoint = DaemonQueryEndpoint::Unix {
        path: socket_path,
        token: "0123456789abcdef0123456789abcdef".to_owned(),
    };

    let started = Instant::now();
    let error = daemon_query_roundtrip(
        &endpoint,
        b"{\"op\":\"ping\"}\n",
        StdDuration::from_millis(60),
        1024,
    )
    .expect_err("dribbling response must time out");

    assert_eq!(
        error
            .downcast_ref::<std::io::Error>()
            .map(std::io::Error::kind),
        Some(std::io::ErrorKind::TimedOut),
        "{error:#}"
    );
    assert!(started.elapsed() < StdDuration::from_millis(300));
    server.join().expect("query server panicked")?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn partial_unix_response_preserves_one_aggregate_byte_limit() -> Result<()> {
    let (mut server, mut client) = UnixStream::pair()?;
    let writer = std::thread::spawn(move || -> Result<()> {
        std::thread::sleep(StdDuration::from_millis(10));
        server.write_all(b"1234")?;
        std::thread::sleep(StdDuration::from_millis(10));
        server.write_all(b"56789")?;
        server.shutdown(Shutdown::Write)?;
        Ok(())
    });

    let error = read_daemon_query_response_unix(&mut client, 8, StdDuration::from_secs(1))
        .expect_err("split response above the aggregate limit must fail");

    assert!(error
        .downcast_ref::<DaemonQueryResponseTooLarge>()
        .is_some());
    writer.join().expect("query response writer panicked")?;
    Ok(())
}

#[cfg(windows)]
fn connect_stalled_query_client(data_root: &Path) -> Result<WindowsQueryHandle> {
    let endpoint = read_daemon_query_endpoint(data_root)?.expect("query endpoint");
    let DaemonQueryEndpoint::WindowsNamedPipe { pipe_name, .. } = endpoint;
    let deadline = WindowsIoDeadline::new(StdDuration::from_secs(2));
    open_windows_daemon_query_pipe(&windows_wide_null(&pipe_name), &deadline)
}

#[cfg(windows)]
fn connect_valid_nonreading_query_client(data_root: &Path) -> Result<WindowsQueryHandle> {
    let endpoint = read_daemon_query_endpoint(data_root)?.expect("query endpoint");
    let DaemonQueryEndpoint::WindowsNamedPipe { pipe_name, token } = endpoint;
    let deadline = WindowsIoDeadline::new(StdDuration::from_secs(2));
    let pipe = open_windows_daemon_query_pipe(&windows_wide_null(&pipe_name), &deadline)?;
    let request = format!(
        "{}\n",
        serde_json::to_string(&compact_json(json!({
            "schema_version": 1,
            "op": "ping",
            "token": token,
        })))?
    );
    write_all_windows_daemon_query_pipe(&pipe, request.as_bytes(), &deadline)?;
    Ok(pipe)
}

#[test]
fn daemon_query_activity_tracks_requests_until_stopping() {
    let activity = Arc::new(DaemonQueryActivity::new());
    let request = activity.begin_request().expect("request accepted");
    let (active, generation) = activity.snapshot();

    assert_eq!(active, 1);

    drop(request);
    let (active, completed_generation) = activity.snapshot();
    assert_eq!(active, 0);
    assert_ne!(completed_generation, generation);
    assert!(activity.begin_request().is_some());
    activity.stop();
    assert!(activity.stopping());
    assert!(activity.begin_request().is_none());
}

#[test]
fn last_query_completion_wakes_the_daemon_waiter() {
    let wakeup = Arc::new(super::daemon_wakeup::DaemonWakeup::default());
    let activity = Arc::new(DaemonQueryActivity::with_idle_wakeup(Arc::clone(&wakeup)));
    let first = activity.begin_request().expect("first request");
    let second = activity.begin_request().expect("second request");
    activity.wake_daemon_when_idle();

    drop(first);
    assert!(wakeup.wait(StdDuration::ZERO).timed_out);

    drop(second);
    assert!(!wakeup.wait(StdDuration::ZERO).timed_out);
}

#[test]
fn ordinary_query_completion_does_not_wake_daemon_maintenance() {
    let wakeup = Arc::new(super::daemon_wakeup::DaemonWakeup::default());
    let activity = Arc::new(DaemonQueryActivity::with_idle_wakeup(Arc::clone(&wakeup)));
    let request = activity.begin_request().expect("ordinary request");

    drop(request);

    assert!(wakeup.wait(StdDuration::ZERO).timed_out);
}

#[cfg(any(unix, windows))]
#[test]
fn stalled_query_client_is_discarded_and_next_query_is_served() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let service = start_test_query_service(temp.path())?;
    let stalled_client = connect_stalled_query_client(temp.path())?;
    wait_for_active_query(&service)?;

    let started = Instant::now();
    let response = daemon_query_request(
        temp.path(),
        compact_json(json!({
            "schema_version": 1,
            "op": "ping",
        })),
        StdDuration::from_secs(2),
        64 * 1024,
    )?
    .expect("query response");

    assert_eq!(response.get("ok").and_then(Value::as_bool), Some(true));
    assert!(started.elapsed() < StdDuration::from_secs(1));
    drop(stalled_client);
    drop(service);
    Ok(())
}

#[cfg(any(unix, windows))]
#[test]
fn query_service_ping_stays_healthy_while_embedder_is_busy() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let runtime = SharedSemanticRuntime::default();
    let wakeup = Arc::new(super::daemon_wakeup::DaemonWakeup::default());
    let handler = ctx_authenticated_request_handler(
        temp.path(),
        runtime.clone(),
        Arc::new(super::source_backed_refresh_adapter::refresh_engine(
            &crate::test_support::CONFIG,
        )),
        wakeup,
        &crate::test_support::CONFIG,
    );
    let service = start_daemon_query_service_with_request_timeout(
        temp.path(),
        handler,
        TEST_QUERY_REQUEST_READ_TIMEOUT,
    )?;
    let _runtime_guard = runtime.lock_for_test()?;

    let started = Instant::now();
    let response = daemon_query_request(
        temp.path(),
        compact_json(json!({
            "schema_version": 1,
            "op": "ping",
        })),
        StdDuration::from_secs(1),
        64 * 1024,
    )?
    .expect("query response");

    assert_eq!(response.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(response.get("busy").and_then(Value::as_bool), Some(true));
    assert!(response["embedding_runtime"].is_null());
    assert!(started.elapsed() < StdDuration::from_millis(500));
    drop(service);
    Ok(())
}

#[cfg(any(unix, windows))]
#[test]
fn authenticated_semantic_intensity_leases_wake_without_changing_query_or_fingerprints(
) -> Result<()> {
    use super::semantic_intensity::SemanticIntensityLeaseRegistry;

    const REQUEST_ID: &str = "019fcaaa-0000-7000-8000-000000000403";

    let temp = tempfile::tempdir()?;
    let wakeup = Arc::new(super::daemon_wakeup::DaemonWakeup::default());
    let registry = Arc::new(SemanticIntensityLeaseRegistry::default());
    let handler = ctx_authenticated_request_handler_with_lifecycle(
        temp.path(),
        SharedSemanticRuntime::default(),
        Arc::new(super::source_backed_refresh_adapter::refresh_engine(
            &crate::test_support::CONFIG,
        )),
        Arc::clone(&wakeup),
        &crate::test_support::CONFIG,
        Arc::new(DaemonLifecycleState::starting()),
        Arc::clone(&registry),
    );
    let query_service = start_daemon_query_service_with_request_timeout(
        temp.path(),
        Arc::clone(&handler),
        TEST_QUERY_REQUEST_READ_TIMEOUT,
    )?;
    let refresh_service = start_daemon_source_refresh_service_with_request_timeout(
        temp.path(),
        handler,
        TEST_QUERY_REQUEST_READ_TIMEOUT,
    )?;
    let query_ping = || {
        daemon_query_request(
            temp.path(),
            compact_json(json!({"schema_version": 1, "op": "ping"})),
            StdDuration::from_secs(1),
            64 * 1024,
        )
    };
    let before_query = query_ping()?.expect("query ping before lease");
    let fingerprint_before = crate::test_support::semantic_contract_fingerprint()?;

    let acquired = daemon_semantic_intensity_lease_request(
        temp.path(),
        SemanticIntensityLeaseOperation::Acquire,
        REQUEST_ID,
        Some(StdDuration::from_secs(5)),
        StdDuration::from_secs(1),
        64 * 1024,
    )?
    .expect("semantic intensity acquire response");
    assert_eq!(acquired["schema_version"], 1, "{acquired}");
    assert_eq!(acquired["lease_status"], "active");
    assert_eq!(acquired["configured_indexing_intensity"], "quiet");
    assert_eq!(acquired["effective_indexing_intensity"], "full");
    assert_eq!(acquired["active_full_intensity_leases"], 1);
    assert_eq!(wakeup.snapshot()["ipc_signals"], 1);
    assert_eq!(
        registry
            .snapshot(SemanticIndexingIntensity::Quiet)
            .effective,
        SemanticIndexingIntensity::Full
    );

    let renewed = daemon_semantic_intensity_lease_request(
        temp.path(),
        SemanticIntensityLeaseOperation::Renew,
        REQUEST_ID,
        Some(StdDuration::from_secs(10)),
        StdDuration::from_secs(1),
        64 * 1024,
    )?
    .expect("semantic intensity renew response");
    assert_eq!(renewed["ttl_ms"], 10_000);
    assert_eq!(wakeup.snapshot()["ipc_signals"], 2);

    let released = daemon_semantic_intensity_lease_request(
        temp.path(),
        SemanticIntensityLeaseOperation::Release,
        REQUEST_ID,
        None,
        StdDuration::from_secs(1),
        64 * 1024,
    )?
    .expect("semantic intensity release response");
    assert_eq!(released["lease_status"], "released");
    assert_eq!(released["effective_indexing_intensity"], "quiet");
    assert_eq!(released["active_full_intensity_leases"], 0);
    assert_eq!(wakeup.snapshot()["ipc_signals"], 3);
    assert!(!daemon_semantic_job_path(temp.path()).exists());
    assert!(!daemon_core_refresh_job_path(temp.path()).exists());

    let after_query = query_ping()?.expect("query ping after lease");
    assert_eq!(before_query["model_key"], after_query["model_key"]);
    assert_eq!(
        fingerprint_before,
        crate::test_support::semantic_contract_fingerprint()?
    );

    let malformed = daemon_source_refresh_request(
        temp.path(),
        json!({
            "schema_version": 1,
            "op": "semantic_intensity_acquire",
            "request_id": REQUEST_ID,
            "ttl_ms": 5_000,
            "persist": true,
        }),
        StdDuration::from_secs(1),
        64 * 1024,
    )?
    .expect("authenticated error response");
    assert_eq!(malformed["ok"], false);
    assert!(malformed["error"]
        .as_str()
        .is_some_and(|error| error.contains("unknown semantic intensity lease request field")));

    drop(query_service);
    drop(refresh_service);
    Ok(())
}

#[cfg(any(unix, windows))]
#[test]
fn query_service_coalesces_source_refresh_requests_on_one_daemon_ticket() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (service, source_refresh) = start_test_source_refresh_service(temp.path())?;
    let request = || {
        daemon_source_refresh_request(
            temp.path(),
            compact_json(json!({
                "schema_version": 1,
                "op": "source_refresh_request",
                "mode": "wait",
                "operation": "refresh",
            })),
            StdDuration::from_secs(1),
            64 * 1024,
        )
    };

    let first = request()?.expect("first source refresh response");
    let second = request()?.expect("coalesced source refresh response");

    assert_eq!(first["request_state"], "admission_pending");
    assert_eq!(second["request_state"], "admission_pending");
    assert_eq!(first["request_id"], second["request_id"]);
    assert_eq!(first["coalesced_requests"], 0);
    assert_eq!(second["coalesced_requests"], 1);
    assert!(source_refresh.has_pending_request());
    let job = read_daemon_job_status(&daemon_source_backed_refresh_job_path(temp.path()))
        .expect("admission-pending source refresh job status");
    assert_eq!(job["owner"], "daemon");
    assert_eq!(job["request_state"], "admission_pending");
    assert_eq!(job["request_id"], first["request_id"]);
    assert_eq!(job["coalesced_requests"], 1);
    drop(service);
    Ok(())
}

#[cfg(any(unix, windows))]
#[test]
fn query_service_shutdown_is_bounded_with_stalled_client() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let service = start_test_query_service(temp.path())?;
    let stalled_client = connect_stalled_query_client(temp.path())?;
    wait_for_active_query(&service)?;

    let started = Instant::now();
    drop(service);

    assert!(started.elapsed() < StdDuration::from_secs(1));
    drop(stalled_client);
    Ok(())
}

#[cfg(any(unix, windows))]
#[test]
fn valid_nonreading_client_does_not_block_later_queries_or_shutdown() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let service = start_test_query_service(temp.path())?;
    let nonreader = connect_valid_nonreading_query_client(temp.path())?;
    std::thread::sleep(StdDuration::from_millis(25));

    let response = daemon_query_request(
        temp.path(),
        compact_json(json!({
            "schema_version": 1,
            "op": "ping",
        })),
        StdDuration::from_secs(2),
        64 * 1024,
    )?
    .expect("query response");
    assert_eq!(response.get("ok").and_then(Value::as_bool), Some(true));

    let started = Instant::now();
    drop(service);
    assert!(started.elapsed() < StdDuration::from_secs(1));
    drop(nonreader);
    Ok(())
}

#[cfg(unix)]
#[test]
fn daemon_query_socket_uses_short_private_fallback_for_long_data_root() -> Result<()> {
    let data_root = PathBuf::from("/tmp").join("x".repeat(160));
    let (listener, path, runtime_dir) = bind_daemon_query_listener(&data_root)?;
    assert!(path.as_os_str().as_bytes().len() <= DAEMON_QUERY_SOCKET_PATH_SAFE_BYTES);
    assert_ne!(path, daemon_query_socket_path(&data_root));
    let runtime_dir = runtime_dir.expect("long path should use a private runtime dir");
    assert_eq!(path.parent(), Some(runtime_dir.as_path()));

    drop(listener);
    fs::remove_file(&path)?;
    fs::remove_dir(&runtime_dir)?;
    Ok(())
}

#[test]
fn daemon_query_request_reader_stops_at_newline() -> Result<()> {
    let mut cursor = std::io::Cursor::new(b"{\"op\":\"ping\"}\nignored".to_vec());

    let body = read_daemon_query_request(&mut cursor, 256)?;

    assert_eq!(body, "{\"op\":\"ping\"}");
    Ok(())
}

#[test]
fn daemon_query_request_reader_rejects_oversized_request() {
    let mut cursor = std::io::Cursor::new(b"abcdef".to_vec());

    let error =
        read_daemon_query_request(&mut cursor, 3).expect_err("oversized request should fail");

    assert!(format!("{error:#}").contains("daemon query request is too large"));
}

#[cfg(unix)]
#[test]
fn daemon_query_endpoint_roundtrips_unix_metadata() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let endpoint = DaemonQueryEndpoint::Unix {
        path: daemon_query_socket_path(temp.path()),
        token: "0123456789abcdef0123456789abcdef".to_owned(),
    };

    write_daemon_query_endpoint(temp.path(), &endpoint)?;
    let loaded = read_daemon_query_endpoint(temp.path())?.expect("endpoint");

    match loaded {
        DaemonQueryEndpoint::Unix { path, token } => {
            assert_eq!(path, daemon_query_socket_path(temp.path()));
            assert_eq!(token, "0123456789abcdef0123456789abcdef");
        }
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn stale_unix_endpoint_is_sanitized_and_removed() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (_socket_root, socket_path) = short_test_query_socket_path()?;
    write_daemon_query_endpoint(
        temp.path(),
        &DaemonQueryEndpoint::Unix {
            path: socket_path.clone(),
            token: "0123456789abcdef0123456789abcdef".to_owned(),
        },
    )?;

    let error = daemon_query_request(
        temp.path(),
        compact_json(json!({"schema_version": 1, "op": "ping"})),
        StdDuration::from_millis(100),
        1024,
    )
    .expect_err("missing socket should be unavailable");
    let message = format!("{error:#}");

    assert!(error
        .downcast_ref::<DaemonQueryServiceUnavailable>()
        .is_some());
    assert!(message.len() < 256, "{message}");
    assert!(!message.contains(&socket_path.display().to_string()));
    assert!(!message.contains("Connection refused"));
    assert!(!message.contains("os error"));
    assert!(!daemon_query_endpoint_path(temp.path()).exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn closed_unix_listener_is_sanitized_and_removed() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (_socket_root, socket_path) = short_test_query_socket_path()?;
    let listener = std::os::unix::net::UnixListener::bind(&socket_path)?;
    drop(listener);
    write_daemon_query_endpoint(
        temp.path(),
        &DaemonQueryEndpoint::Unix {
            path: socket_path.clone(),
            token: "0123456789abcdef0123456789abcdef".to_owned(),
        },
    )?;

    let error = daemon_query_request(
        temp.path(),
        compact_json(json!({"schema_version": 1, "op": "ping"})),
        StdDuration::from_millis(100),
        1024,
    )
    .expect_err("closed listener should be unavailable");
    let message = format!("{error:#}");
    assert!(error
        .downcast_ref::<DaemonQueryServiceUnavailable>()
        .is_some());
    assert!(!message.contains(&socket_path.display().to_string()));
    assert!(!message.contains("Connection refused"));
    assert!(!message.contains("os error"));
    assert!(!daemon_query_endpoint_path(temp.path()).exists());
    fs::remove_file(socket_path)?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn unavailable_cleanup_preserves_replacement_endpoint_identity() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let stale_endpoint = DaemonQueryEndpoint::Unix {
        path: temp.path().join("stale.sock"),
        token: "0123456789abcdef0123456789abcdef".to_owned(),
    };
    write_daemon_query_endpoint(temp.path(), &stale_endpoint)?;
    let observed =
        read_daemon_query_endpoint_identity(temp.path())?.expect("stale endpoint identity");
    let replacement_owner_pid = process::id().saturating_add(1).max(1);
    let replacement_path = temp.path().join("replacement.sock");
    write_private_json_file(
        &daemon_query_endpoint_path(temp.path()),
        &compact_json(json!({
            "schema_version": 1,
            "transport": "unix",
            "path": replacement_path,
            "token": "fedcba9876543210fedcba9876543210",
            "pid": replacement_owner_pid,
        })),
    )?;

    remove_daemon_query_endpoint_if_matches(temp.path(), &observed);

    let current = read_daemon_query_endpoint_identity(temp.path())?.expect("replacement endpoint");
    assert_eq!(current.owner_pid, replacement_owner_pid);
    assert_ne!(current, observed);
    Ok(())
}

#[cfg(unix)]
#[test]
fn unavailable_cleanup_waits_for_replacement_daemon_ownership() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let endpoint = DaemonQueryEndpoint::Unix {
        path: temp.path().join("owned-replacement.sock"),
        token: "0123456789abcdef0123456789abcdef".to_owned(),
    };
    write_daemon_query_endpoint(temp.path(), &endpoint)?;
    let observed = read_daemon_query_endpoint_identity(temp.path())?.expect("endpoint identity");
    let replacement_daemon =
        DaemonLock::acquire(temp.path())?.expect("replacement daemon should acquire ownership");

    remove_daemon_query_endpoint_if_matches(temp.path(), &observed);

    assert_eq!(
        read_daemon_query_endpoint_identity(temp.path())?,
        Some(observed)
    );
    drop(replacement_daemon);
    Ok(())
}

#[cfg(unix)]
#[test]
fn unix_pre_submission_disconnect_kinds_are_classified_as_unavailable() {
    for kind in [
        std::io::ErrorKind::NotFound,
        std::io::ErrorKind::ConnectionRefused,
        std::io::ErrorKind::ConnectionReset,
        std::io::ErrorKind::ConnectionAborted,
        std::io::ErrorKind::BrokenPipe,
        std::io::ErrorKind::NotConnected,
    ] {
        assert!(daemon_query_unix_io_error_is_pre_submission_unavailable(
            kind
        ));
    }
    assert!(!daemon_query_unix_io_error_is_pre_submission_unavailable(
        std::io::ErrorKind::TimedOut
    ));
}

#[test]
fn windows_pre_submission_disconnect_codes_are_classified_without_native_io() {
    for raw_os_error in [2, 3, 109, 230, 232, 233] {
        assert!(daemon_query_windows_io_error_is_pre_submission_unavailable(
            std::io::ErrorKind::Other,
            Some(raw_os_error),
        ));
    }
    assert!(
        !daemon_query_windows_io_error_is_pre_submission_unavailable(
            std::io::ErrorKind::TimedOut,
            None,
        )
    );
}

#[test]
fn windows_pipe_creation_source_verifies_a_protected_handle_bound_acl() {
    let identity = include_str!("../../ctx-daemon-runtime/src/windows_identity.rs");
    let security = include_str!("../../ctx-daemon-runtime/src/ipc/server/windows_security.rs");
    let server = include_str!("../../ctx-daemon-runtime/src/ipc/server/transport.rs");
    assert!(identity.contains("OpenProcessToken(GetCurrentProcess()"));
    assert!(identity.contains("GetTokenInformation"));
    assert!(identity.contains("TokenUser"));
    assert!(security.contains("CurrentProcessTokenUser::current()"));
    assert!(security.contains("WinLocalSystemSid"));
    assert!(security.contains("SE_DACL_PROTECTED"));
    assert!(security.contains("GetSecurityInfo"));
    assert!(security.contains("OWNER_SECURITY_INFORMATION"));
    assert!(server.contains("for_current_user_and_system"));
    assert!(server.contains(".verify_handle(pipe.handle)"));
    assert!(server.contains("&security_attributes"));
}

#[cfg(windows)]
#[test]
fn daemon_query_endpoint_roundtrips_windows_named_pipe_metadata() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let pipe_name = daemon_query_pipe_name();
    assert!(windows_named_pipe_name_is_local(&pipe_name));
    let endpoint = DaemonQueryEndpoint::WindowsNamedPipe {
        pipe_name: pipe_name.clone(),
        token: "0123456789abcdef0123456789abcdef".to_owned(),
    };

    write_daemon_query_endpoint(temp.path(), &endpoint)?;
    let loaded = read_daemon_query_endpoint(temp.path())?.expect("endpoint");

    match loaded {
        DaemonQueryEndpoint::WindowsNamedPipe {
            pipe_name: loaded_pipe_name,
            token,
        } => {
            assert_eq!(loaded_pipe_name, pipe_name);
            assert_eq!(token, "0123456789abcdef0123456789abcdef");
        }
    }
    Ok(())
}

#[cfg(windows)]
#[test]
fn daemon_query_endpoint_rejects_nonlocal_windows_pipe_name() -> Result<()> {
    let temp = tempfile::tempdir()?;
    create_private_dir_all(&daemon_root_path(temp.path()))?;
    let endpoint = compact_json(json!({
        "schema_version": 1,
        "transport": "windows_named_pipe",
        "pipe_name": r"\\server\pipe\ctx-daemon-query-0123456789abcdef0123456789abcdef",
        "token": "0123456789abcdef0123456789abcdef",
    }));
    write_private_json_file(&daemon_query_endpoint_path(temp.path()), &endpoint)?;

    assert!(read_daemon_query_endpoint(temp.path())?.is_none());
    Ok(())
}

#[test]
fn daemon_query_endpoint_rejects_short_tokens() -> Result<()> {
    let temp = tempfile::tempdir()?;
    create_private_dir_all(&daemon_root_path(temp.path()))?;
    let mut endpoint = compact_json(json!({
            "schema_version": 1,
            "transport": "unix",
            "token": "short",
    }));
    #[cfg(unix)]
    {
        endpoint["path"] = Value::String(
            daemon_query_socket_path(temp.path())
                .to_string_lossy()
                .into_owned(),
        );
    }
    #[cfg(windows)]
    {
        endpoint["transport"] = Value::String("windows_named_pipe".to_owned());
        endpoint["pipe_name"] = Value::String(daemon_query_pipe_name());
    }
    write_private_json_file(&daemon_query_endpoint_path(temp.path()), &endpoint)?;

    assert!(read_daemon_query_endpoint(temp.path())?.is_none());
    Ok(())
}
